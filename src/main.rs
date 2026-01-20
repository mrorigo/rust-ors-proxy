use axum::{
    routing::{get, post},
    Json, Router,
};
use std::net::SocketAddr;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

mod types;

#[tokio::main]
async fn main() {
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new(
            std::env::var("RUST_LOG").unwrap_or_else(|_| "info".into()),
        ))
        .with(tracing_subscriber::fmt::layer())
        .init();

    let app = Router::new()
        .route("/health", get(health_check))
        .route("/v1/responses", post(create_response));

    let addr = SocketAddr::from(([0, 0, 0, 0], 3000));
    tracing::info!("listening on {}", addr);
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

async fn health_check() -> &'static str {
    "OK"
}

use axum::response::IntoResponse;
async fn create_response(Json(payload): Json<types::OrsRequest>) -> impl IntoResponse {
    tracing::info!("Received request: {:?}", payload);
    // Placeholder: return 200 OK with empty JSON for now
    axum::Json(serde_json::json!({ "status": "received" }))
}
