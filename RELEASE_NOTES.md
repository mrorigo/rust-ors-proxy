# Release Notes - v0.1.0: The Canonical Bridge 🦀

We are thrilled to announce the first release of the **Rust ORS Proxy**, the definitive high-performance bridge for adopting the **Open Responses Specification (ORS)** today. 

This release empowers developers to write modern, agentic code against the ORS standard *right now*, while deploying on ubiquitous legacy infrastructure like OpenAI, Ollama, and vLLM.

## 🌟 Highlights

### ⚡️ Strict ORS Compliance
We have achieved full pixel-perfect compliance with the **Jan 2026 Open Responses Specification**, enabling robust and type-safe client implementations:
- **Typed Content Parts**: Full support for `response.content_part.added` and `response.content_part.done` lifecycles.
- **Stream Integrity**: Every event carries a monotonic `sequence_number` and precise `output_index`/`content_index` for reliable reconstruction.
- **Rich Events**: Deprecated the "loose" JSON of the past in favor of strictly typed, stateful event streams.

### 🛠️ Bridge the Legacy Gap
- **Universal Tool Support**: The proxy automatically transcodes messy, provider-specific `tool_calls` chunks into clean, standardized `function_call` output items. Writes agents, not parsing regexes.
- **Multimodal Ready**: Seamlessly maps ORS Image input types to upstream `gpt-4-vision` compatible payload formats.

### 💾 Stateful Context Management
- **Stateless Clients**: The proxy includes an embedded **SQLite** engine that automatically persists and hydrates conversation history. 
- **Context Replay**: Pass a `previous_response_id` and the proxy reconstructs the full context window for the upstream provider instantly.

### 🚀 Production Engineered
- Built on the reliability of **Rust**, **Tokio**, and **Axum**.
- **Resilient Streaming**: Custom `SseCodec` implementation handles network fragmentation, newline splits, and upstream buffering anomalies without dropping a token.
- **Zero-Config**: Points to `http://localhost:11434/v1/chat/completions` by default. It just works.

## 📦 Changes
- Initial Release (v0.1.0)
- Implemented `Transcoder` for strict SSE event mapping.
- Added in-memory and file-based SQLite support.
- Added support for `message` and `function_call` item types.
- Added comprehensive unit testing suite.
- Published strict CI workflows.

## 🤝 Getting Started

Clone and run in seconds:

```bash
git clone https://github.com/mrorigo/rust-ors-proxy
cd rust-ors-proxy
cargo run
```

*The future won't wait. Neither should you.*
