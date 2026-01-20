For a high‑throughput, short‑lived “ORS → legacy Chat Completions” migration proxy, a lean Tokio + hyper/Axum stack is still the pragmatic sweet spot, but with a few “drop the framework where it hurts” choices: Axum for routing and SSE surface, hyper directly for the hot streaming path, raw SQL with rusqlite/sqlx, and very simple Serde enums for ORS polymorphism. [tokio](https://tokio.rs)

***

## 1. Async runtime and HTTP/SSE stack

For an I/O‑bound proxy that must bridge SSE, Tokio remains the best option because:

- It gives the most optimized multi‑threaded scheduler and networking stack, and most ecosystem crates (hyper, Axum, sqlx, reqwest, etc.) are built around it. [github](https://github.com/tokio-rs/tokio)
- Axum’s SSE support and ergonomics are good, but pure hyper still benchmarks noticeably faster at the HTTP layer (around 20–30% better req/s and significantly lower latency in framework benchmarks). [blog.nashtechglobal](https://blog.nashtechglobal.com/rust-htmx-and-sse/)

Concrete pattern:

- Use **Tokio multi-threaded runtime** with tuned worker count (num_cpus or slightly under) and disable unnecessary features.  
- Expose endpoints with **Axum** for:
  - Request parsing/validation into your ORS types.
  - Non‑streaming responses and health endpoints.
- For the streaming path:
  - Accept the request via Axum, but hand off to a **hyper client** (or `reqwest` with streaming body) that maintains a single connection per upstream request and yields chunks as `Stream<Item = Result<Bytes, E>>`. [stackoverflow](https://stackoverflow.com/questions/75641001/what-is-the-optimal-way-to-make-external-network-requests-using-axum-tokio-and)
  - Emit SSE from Axum using `Sse<impl Stream<Item = Result<Event, Infallible>>>`, but keep the stream body extremely thin: just a state machine over upstream chunks.

If you want to go even leaner than Axum:

- Run a **pure hyper server** for the hot SSE endpoint and only use Axum (or another framework) for “slow‑path” ops on a separate port/process.  
- This gives closer to raw hyper performance, avoiding Axum’s extra extractors and layers on the hot path. [github](https://github.com/tokio-rs/axum/discussions/2566)

Given your constraints (ephemeral infra, minimal boilerplate) a single binary with:

- Tokio runtime  
- Axum router  
- hyper client  

is likely the best cost/complexity/perf trade‑off.

***

## 2. Streaming state machine for ORS lifecycle

You want to:

- Accept ORS streaming requests.  
- Call upstream Chat Completions (possibly OpenAI‑style `stream: true`).  
- Buffer legacy deltas and synthesize ORS lifecycle events, especially `output_item.added` before content bytes arrive.

Lightweight pattern:

- Define a **small enum + struct** state machine that takes “upstream chunk” → “zero or more ORS SSE events”.

Example conceptual states:

- **Init**:  
  - On first client subscription, immediately emit ORS `response.output_item.added` with an item id and metadata but empty content array.  
  - Optionally emit `response.created` or similar “header” events if your ORS flavor wants that before tokens.  
- **Streaming**:
  - For each upstream delta:
    - Parse minimally: only extract role, text delta, tool call chunks, finish reason.
    - Accumulate per “output item” buffer in memory, but keep it append‑only.
    - For each delta, emit a matching ORS SSE event (e.g. `response.output_text.delta` or `response.output_item.delta`) referencing the earlier item id.  
- **Done**:
  - When upstream signals completion (finish_reason, closed stream, or error):
    - Emit `response.completed` or `response.error` events as per ORS.  

Implementation tactics to keep it lean:

- Use **incremental JSON parsing** only where needed:
  - Treat upstream chunks as arbitrary byte slices until a full JSON line/object boundary is seen (OpenAI‑style SSE is newline‑terminated JSON per `data:`).  
  - Use a tiny line/record accumulator (`Vec<u8>` + `memchr` for `\n`) and `serde_json::from_slice` only on complete JSON objects, avoiding building an AST of the whole response. [stackoverflow](https://stackoverflow.com/questions/75641001/what-is-the-optimal-way-to-make-external-network-requests-using-axum-tokio-and)
- Model the state machine as:

  ```rust
  enum StreamState {
      Init,
      Streaming { current_item_id: String },
      Done,
  }
  ```

  with a `fn on_upstream_chunk(&mut self, chunk: &Bytes) -> Vec<OrsEvent>` method that yields ORS events for each upstream delta.

- To inject `output_item.added` before content:
  - On transition from `Init` to first `Streaming`, allocate a random/monotonic id and emit that event, before any delta that has text or function_call content.

This keeps overhead to allocator + minimal JSON decode per upstream `data:` line and avoids heavier combinators or layered streams.

***

## 3. Raw SQL for `store: true` context replay

For context replay with SQLite and **no ORM**, a straightforward pattern is:

- One `conversations` table and one `messages` table, both with compact, indexed columns.
- Store only what is needed to reconstruct an upstream Chat Completions payload.

Example schema (sqlite):

```sql
CREATE TABLE conversations (
    id          TEXT PRIMARY KEY,
    created_at  INTEGER NOT NULL
);

CREATE TABLE messages (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    conversation_id TEXT NOT NULL,
    role            TEXT NOT NULL,
    content_json    TEXT NOT NULL,
    created_at      INTEGER NOT NULL,
    FOREIGN KEY (conversation_id) REFERENCES conversations(id)
);

CREATE INDEX idx_messages_conversation_created
    ON messages(conversation_id, created_at);
```

Patterns:

- Use `rusqlite` for **sync DB** in a dedicated worker thread, or `sqlx` with `sqlite` feature for async; both work well with Tokio and do not require ORM layers. [leapcell](https://leapcell.io/blog/navigating-the-asynchronous-landscape-a-deep-dive-into-async-std-and-tokio)
- When `store: true` and ORS context includes a `conversation_id`:
  - Before calling upstream, run:

    ```sql
    SELECT role, content_json
    FROM messages
    WHERE conversation_id = ?
    ORDER BY created_at ASC;
    ```

  - Deserialize `content_json` to your internal `MessageContent` enum or pass straight through as `serde_json::Value` if you only need to re‑encode into Chat Completions.  
  - Prepend this history to the new user message to build the upstream `messages` array.  

- After upstream completion:
  - Insert the new user message and assistant message(s) in a simple transaction (BEGIN; INSERT …; COMMIT;).  
- Keep all SQL calls in **small, explicit helper fns**:

  ```rust
  fn load_history(conn: &Connection, conv_id: &str) -> Result<Vec<StoredMessage>> { ... }
  fn append_messages(conn: &Connection, conv_id: &str, msgs: &[StoredMessage]) -> Result<()> { ... }
  ```

That keeps logic auditable, minimizes abstraction overhead, and avoids an ORM while still giving indexable history.

***

## 4. Serde patterns for ORS polymorphism

Goal: strictly typed ORS items (e.g. `input_text` vs `function_call`) while tolerating loose upstream JSON.

Recommended patterns:

- Define **strong, tagged enums** for ORS‑facing types:

  ```rust
  #[derive(Serialize, Deserialize)]
  #[serde(tag = "type", rename_all = "snake_case")]
  enum OrsInputItem {
      InputText { text: String },
      FunctionCall { name: String, arguments: serde_json::Value },
  }
  ```

  This uses Serde’s internally tagged representation and will reject invalid shapes at the ORS boundary. [serde](https://serde.rs/enum-representations.html)

- For upstream Chat Completions, use **looser types**:

  ```rust
  #[derive(Deserialize)]
  struct LegacyDelta {
      #[serde(default)]
      role: Option<String>,
      #[serde(default)]
      content: Option<Vec<LegacyContentPiece>>,
      #[serde(default)]
      tool_calls: Option<Vec<LegacyToolCall>>,
      #[serde(default)]
      finish_reason: Option<String>,
      // keep remainder for forwards-compat:
      #[serde(flatten)]
      extra: serde_json::Value,
  }
  ```

  with `LegacyContentPiece` also using `#[serde(untagged)]` when OpenAI‑style `content` can be either a simple text string or a richer object. [stackoverflow](https://stackoverflow.com/questions/78507919/serde-rs-deserialize-enums-with-different-content)

- Bridge function:

  - `fn legacy_delta_to_ors_events(delta: LegacyDelta) -> Vec<OrsEvent>` maps the fuzzy upstream type into your strict ORS enums.
  - Any unknown fields remain in `extra` and can be ignored or logged, ensuring proxy robustness.

To manage polymorphic *content* arrays in ORS:

```rust
#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum OrsContent {
    OutputText { text: String },
    FunctionCall { name: String, arguments: serde_json::Value },
}
```

Then in the ORS item:

```rust
#[derive(Serialize, Deserialize)]
struct OrsOutputItem {
    id: String,
    #[serde(default)]
    content: Vec<OrsContent>,
}
```

The strictness is at the ORS surface (fully typed enums); the “unsafe” side is constrained to a small mapping module that takes `serde_json::Value` or permissive structs and either:

- Produces valid ORS enums, or  
- Fails fast with a proxied error response.

This keeps boilerplate low and ensures your migration proxy is just a small Tokio/Axum/hyper service with:

- A fast async runtime.  
- A tiny streaming state machine for SSE bridging.  
- Raw‑SQL context replay in SQLite.  
- Strongly typed ORS models over a loose legacy JSON interface.