This document outlines the technical specification for the **Open Responses Proxy (ORP)**.

**Project Goal:** Construct a high-performance, ephemeral "shim" service in Rust that allows frontend clients to adopt the Open Responses Specification (ORS) immediately, while the backend relies on a legacy Chat Completions provider (e.g., Azure OpenAI, vLLM, or legacy OpenAI endpoints).

**Core Philosophy:** Maximum throughput, strict typing at the edge, loose coupling upstream, and zero "framework bloat."

---

## 1. System Architecture

The proxy acts as a translation layer. It accepts stateful, strictly typed ORS requests, manages context persistence locally (Context Replay), and streams stateless legacy Chat Completion requests upstream.

*   **Runtime:** **Tokio** (Multi-threaded). The standard for I/O-bound proxies.
*   **Server Interface:** **Axum**. Selected for robust routing and ergonomic Server-Sent Events (SSE) support.
*   **Upstream Client:** **Hyper** (via `reqwest` for minimal boilerplate). Handles the hot streaming path to the legacy provider.
*   **State Store:** **SQLite** (via `sqlx` or `rusqlite`). Embedded, zero-latency database to handle `store: true` logic without external dependencies like Redis.

---

## 2. Domain Model & Type Definitions

We utilize Rust’s type system to enforce ORS strictness at the entry gate, while using permissive types for the legacy upstream response to prevent crashes on unexpected data.

### 2.1. Inbound (Strict ORS Enums)
Use `serde` with internal tagging to validate polymorphic items automatically.

```rust
use serde::{Deserialize, Serialize};
use serde_json::Value;

// The top-level payload for POST /v1/responses
#[derive(Deserialize, Debug)]
pub struct OrsRequest {
    pub model: String,
    pub input: Vec<OrsInputItem>, // Polymorphic input array
    pub store: bool, // Triggers context replay logic
    pub previous_response_id: Option<String>,
    #[serde(default)]
    pub stream: bool,
}

// Strict Item Polymorphism
#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OrsInputItem {
    Message {
        role: OrsRole,
        content: Vec<OrsContentPart>,
    },
    FunctionCall {
        call_id: String,
        name: String,
        arguments: Value, // Keep generic for storage
    },
    FunctionCallOutput {
        call_id: String,
        output: String, // or Value
    },
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "snake_case")]
pub enum OrsRole {
    User,
    Assistant,
    Developer, // Must map to 'system' for legacy
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OrsContentPart {
    InputText { text: String },
    InputImage { image_url: Value },
}
```

### 2.2. Upstream (Loose Legacy Structs)
We treat the upstream strictly as a source of raw data chunks to be transcoded.

```rust
// The Legacy Upstream Request
#[derive(Serialize)]
pub struct LegacyChatRequest {
    pub model: String,
    pub messages: Vec<LegacyMessage>,
    pub stream: bool,
}

#[derive(Serialize)]
pub struct LegacyMessage {
    pub role: String,
    pub content: Option<String>, // Simplification for text-only upstream
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<Value>>,
}

// The Legacy Stream Chunk
#[derive(Deserialize, Debug)]
pub struct LegacyChunk {
    pub choices: Vec<LegacyChoice>,
}

#[derive(Deserialize, Debug)]
pub struct LegacyChoice {
    pub delta: LegacyDelta,
    pub finish_reason: Option<String>,
}

#[derive(Deserialize, Debug)]
pub struct LegacyDelta {
    pub content: Option<String>,
    pub tool_calls: Option<Vec<Value>>, 
    // Capture unknown fields in 'extra' to prevent errors
    #[serde(flatten)]
    pub extra: Value, 
}
```

---

## 3. Storage Layer (Context Replay)

Since legacy providers are stateless, the proxy must "hydrate" the request by fetching history if `previous_response_id` is present. We use raw SQL for performance and transparency.

**Schema (SQLite):**
```sql
CREATE TABLE conversations (
    id TEXT PRIMARY KEY, 
    created_at INTEGER NOT NULL
);

-- Store flattened items to allow linear reconstruction
CREATE TABLE items (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    conversation_id TEXT NOT NULL,
    sequence_index INTEGER NOT NULL,
    item_type TEXT NOT NULL, -- 'message', 'function_call', etc.
    payload JSON NOT NULL,   -- The full OrsInputItem serialized
    FOREIGN KEY(conversation_id) REFERENCES conversations(id)
);
CREATE INDEX idx_items_seq ON items(conversation_id, sequence_index);
```

**Access Logic:**
1.  **Fetch:** `SELECT payload FROM items WHERE conversation_id = ? ORDER BY sequence_index ASC`.
2.  **Deserialize:** Convert JSON blobs back into `Vec<OrsInputItem>`.
3.  **Append:** Add the *new* items from the current request to this vector.
4.  **Translate:** Map the full vector to `LegacyMessage` structs (e.g., `Developer` -> `System`, `FunctionCallOutput` -> `Tool` role).

---

## 4. Streaming Core: The Event Transcoder

This is the most critical component. It converts "dumb" chunks into "semantic" lifecycle events.

### 4.1. The Events
The ORS client expects Server-Sent Events (SSE) with specific event names.

```rust
#[derive(Serialize)]
#[serde(tag = "event", content = "data")] // Custom serializer needed for SSE format
pub enum OrsEvent {
    #[serde(rename = "response.created")]
    Created { id: String },
    
    #[serde(rename = "response.output_item.added")]
    ItemAdded { 
        item_id: String, 
        #[serde(rename = "type")]
        item_type: String 
    },
    
    #[serde(rename = "response.output_text.delta")]
    TextDelta { 
        item_id: String, 
        delta: String 
    },
    
    #[serde(rename = "response.output_item.done")]
    ItemDone { item_id: String, status: String },
}
```

### 4.2. The State Machine
We use a buffering state machine to handle the mismatch between Legacy (Content-first) and ORS (Structure-first).

**Logic:**
1.  **State `Init`:** Receive first chunk.
2.  **Action:** Generate a new UUID (`item_id`).
3.  **Synthetic Emission:** Immediately emit `response.output_item.added`. *Crucial step:* This prevents the "flash of content" on the frontend.
4.  **State `Streaming`:**
    *   If `delta.content` exists -> Emit `response.output_text.delta`.
    *   If `finish_reason` exists -> Emit `response.output_item.done` and transition to `Done`.
5.  **Side Effect:** Accumulate the full content string in memory to save to SQLite once the stream finishes.

---

## 5. API Specification

**Endpoint:** `POST /v1/responses`

**Headers:**
*   `Authorization`: Bearer `[OPENAI_API_KEY]` (Pass-through or internal proxy key).
*   `Content-Type`: `application/json`.

**Implementation Sketch (Axum Handler):**

```rust
async fn responses_handler(
    State(state): State<AppState>,
    Json(payload): Json<OrsRequest>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    
    // 1. Context Management (Async/Blocking offload)
    let history = if let Some(prev_id) = payload.previous_response_id {
        state.db.load_context(&prev_id).await.unwrap()
    } else {
        Vec::new()
    };
    
    // 2. Transformation
    let legacy_messages = transform_ors_to_legacy(history, &payload.input);
    
    // 3. Upstream Call (Hyper/Reqwest)
    let upstream_stream = state.client.post(UPSTREAM_URL)
        .json(&LegacyChatRequest { 
            messages: legacy_messages, 
            stream: true, 
            .. 
        })
        .send()
        .await
        .unwrap()
        .bytes_stream();

    // 4. Transcode Stream (The "Shim" Logic)
    let stream = try_stream! {
        let mut transcoder = Transcoder::new();
        
        // Yield generic "Created" event
        yield Event::default().event("response.created").json_data(json!({"id": "resp_123"})).unwrap();

        for await chunk in upstream_stream {
            let chunk = chunk.unwrap(); // Handle errors properly in prod
            let legacy_data: LegacyChunk = parse_sse_chunk(&chunk);
            
            // Transcoder generates Vec<OrsEvent> from one LegacyChunk
            let events = transcoder.process(legacy_data); 
            
            for event in events {
                yield event.to_sse(); // Convert internal enum to Axum SSE Event
            }
        }
        
        // 5. Async Save (Fire and Forget or Await based on consistency needs)
        state.db.save_interaction(payload.input, transcoder.get_accumulated_output()).await;
    };

    Sse::new(stream).keep_alive(KeepAlive::default())
}
```

---

## 6. Implementation Plan

This project targets the "pragmatic sweet spot" defined in the research:

1.  **Phase 1: The Core (No DB):**
    *   Set up `Axum` and the `serde` types.
    *   Implement the `Transcoder` state machine.
    *   Verify that `curl localhost:3000/v1/responses` returns valid SSE events (`added` before `delta`) when mocking the upstream.

2.  **Phase 2: The Bridge:**
    *   Integrate `reqwest` to hit the actual OpenAI/vLLM legacy endpoint.
    *   Implement `transform_ors_to_legacy` (flattening tool outputs into messages).

3.  **Phase 3: State (SQLite):**
    *   Add `sqlx` with the schema defined above.
    *   Implement logic to construct the full conversation context on every request.
    *   *Note:* Ensure the SQLite connection is strictly behind a `Arc<Mutex<>>` or connection pool to handle async concurrency safely.

4.  **Phase 4: Hardening:**
    *   Add error handling for "Refusals" (Legacy might send refusal text; ORS expects specific refusal items).
    *   Ensure `function_call` parsing handles fragmented JSON streaming if the upstream splits JSON tokens across chunks.
