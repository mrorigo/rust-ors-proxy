use axum::{
    extract::State,
    response::{sse::{Event, KeepAlive}, Sse, IntoResponse},
    routing::{get, post},
    Json, Router,
};
use futures::stream::Stream;
use reqwest::Client;
use std::{net::SocketAddr, sync::Arc};
use tokio_stream::StreamExt;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use uuid::Uuid;

mod types;
mod transcoder;
mod upstream;
mod db;
mod sse_codec;

// use types::{LegacyChatRequest, LegacyChunk}; // Removed unused imports
// Wait, I named it LegacyChatRequest in types.rs. 
// Ah, allow me to double check types.rs content via `view_file` to be sure before writing.
// However, I can't view file in middle of tool call. 
// I recall defining it as LegacyChatRequest.

#[derive(Clone)]
struct AppState {
    client: Client,
    upstream_url: String,
    openai_api_key: Option<String>,
    db: Arc<db::Db>,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new(
            std::env::var("RUST_LOG").unwrap_or_else(|_| "info".into()),
        ))
        .with(tracing_subscriber::fmt::layer())
        .init();

    // Load env vars
    let upstream_url = std::env::var("UPSTREAM_URL")
        .unwrap_or_else(|_| "http://localhost:11434/v1/chat/completions".to_string());
    let openai_api_key = std::env::var("OPENAI_API_KEY").ok();
    let database_url = std::env::var("DATABASE_URL") // Default to explicit file or in-memory?
        .unwrap_or_else(|_| "sqlite://ors_proxy.db?mode=rwc".to_string());

    let db = db::Db::new(&database_url).await.expect("Failed to init DB");

    let state = AppState {
        client: Client::new(),
        upstream_url,
        openai_api_key,
        db: Arc::new(db),
    };

    let app = Router::new()
        .route("/health", get(health_check))
        .route("/v1/responses", post(create_response))
        .with_state(state);

    let addr = SocketAddr::from(([0, 0, 0, 0], 3000));
    tracing::info!("listening on {}", addr);
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

async fn health_check() -> &'static str {
    "OK"
}

async fn create_response(
    State(state): State<AppState>,
    Json(payload): Json<types::OrsRequest>,
) -> impl IntoResponse {
    tracing::info!("Received request for model: {}", payload.model);

    // 1. Context Management
    let conversation_id = payload.previous_response_id
        .clone()
        .unwrap_or_else(|| Uuid::new_v4().to_string());

    let mut full_input = if payload.previous_response_id.is_some() {
        match state.db.load_context(&conversation_id).await {
            Ok(history) => history,
            Err(e) => {
                tracing::error!("Failed to load context: {}", e);
                return axum::response::Response::builder()
                    .status(500)
                    .body(axum::body::Body::from("Failed to load context"))
                    .unwrap();
            }
        }
    } else {
        Vec::new()
    };
    
    // Append current input
    full_input.extend(payload.input.clone());

    // 2. Transform request with FULL history
    let legacy_messages = upstream::transform_ors_to_legacy(full_input); // Use full_input here!

    let legacy_req = types::LegacyChatRequest {
        model: payload.model,
        messages: legacy_messages,
        stream: true,
    };

    // 3. Prepare upstream request
    let mut req_builder = state.client.post(&state.upstream_url)
        .json(&legacy_req);
    
    if let Some(key) = &state.openai_api_key {
        req_builder = req_builder.bearer_auth(key);
    }

    // 4. Execute request
    let res = match req_builder.send().await {
        Ok(res) => res,
        Err(e) => {
            tracing::error!("Upstream error: {}", e);
            return axum::response::Response::builder()
                .status(502)
                .body(axum::body::Body::from(format!("Upstream error: {}", e)))
                .unwrap(); 
        }
    };

    if !res.status().is_success() {
         let error_text = res.text().await.unwrap_or_default();
         tracing::error!("Upstream failed: {}", error_text);
         
         let error_body = serde_json::json!({
             "error": {
                 "message": format!("Upstream provider error: {}", error_text),
                 "type": "upstream_error",
                 "code": "upstream_failed"
             }
         });
         
         return axum::response::Response::builder()
                .status(502) // Bad Gateway
                .header("Content-Type", "application/json")
                .body(axum::body::Body::from(error_body.to_string()))
                .unwrap();
    }

    // 5. Stream and Transcode (and Save)
    let stream = make_stream(res, state, conversation_id, payload.input);

    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

fn make_stream(
    res: reqwest::Response,
    state: AppState,
    conversation_id: String,
    input_items: Vec<types::OrsInputItem>
) -> impl Stream<Item = Result<Event, std::io::Error>> {
    async_stream::try_stream! {
        let mut upstream_stream = res.bytes_stream();
        let mut transcoder = transcoder::Transcoder::new();
        let mut accumulated_events: Vec<types::OrsEvent> = Vec::new();
        let mut codec = sse_codec::SseCodec::new();
        
        while let Some(chunk_result) = upstream_stream.next().await {
            let chunk_bytes = chunk_result.map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
            
            // Use codec to extract complete lines
            let lines = codec.decode(chunk_bytes);
            
            for line in lines {
                let line = line.trim();
                if line.starts_with("data: ") {
                    let json_str = &line["data: ".len()..];
                    if json_str == "[DONE]" {
                        continue;
                    }
                    
                    if let Ok(legacy_chunk) = serde_json::from_str::<types::LegacyChunk>(json_str) {
                        let events = transcoder.process(legacy_chunk);
                        for event in events {
                            // Accumulate for storage
                            accumulated_events.push(event.clone());

                            let sse_event = Event::default()
                                .event(event_name(&event))
                                .json_data(&event)
                                .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
                            
                            yield sse_event;
                        }
                    } else {
                        tracing::warn!("Failed to parse legacy chunk: {}", json_str);
                    }
                }
            }
        }
        
        // Post-stream persistence
        if let Err(e) = state.db.save_interaction(&conversation_id, input_items, accumulated_events).await {
             tracing::error!("Failed to save interaction: {}", e);
        }
    }
}

fn event_name(event: &types::OrsEvent) -> &'static str {
    match event {
        types::OrsEvent::Created { .. } => "response.created",
        types::OrsEvent::ItemAdded { .. } => "response.output_item.added",
        types::OrsEvent::ContentPartAdded { .. } => "response.content_part.added",
        types::OrsEvent::TextDelta { .. } => "response.output_text.delta",
        types::OrsEvent::FunctionCallArgumentsDelta { .. } => "response.function_call_arguments.delta",
        types::OrsEvent::ContentPartDone { .. } => "response.content_part.done",
        types::OrsEvent::ItemDone { .. } => "response.output_item.done",
    }
}

