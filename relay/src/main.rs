mod auth;
mod bridge;
mod registry;
mod types;

use axum::{routing::get, Router};
use bridge::AppState;
use registry::Registry;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("sidekar_relay=info".parse().unwrap()))
        .init();

    // Required env vars
    let mongodb_uri =
        std::env::var("MONGODB_URI").expect("MONGODB_URI environment variable is required");
    let jwt_secret =
        std::env::var("JWT_SECRET").expect("JWT_SECRET environment variable is required").trim().to_string();
    let port = std::env::var("PORT").unwrap_or_else(|_| "8080".into());

    // Connect to MongoDB
    let client = mongodb::Client::with_uri_str(&mongodb_uri)
        .await
        .expect("failed to connect to MongoDB");
    let db = client.database("sidekar");
    tracing::info!("connected to MongoDB");

    // Create registry
    let registry = Registry::new();

    // App state
    let state = AppState {
        db,
        registry,
        jwt_secret,
    };

    // CORS — allow sidekar.dev
    let cors = CorsLayer::new()
        .allow_origin(AllowOrigin::list([
            "https://sidekar.dev".parse().unwrap(),
            "http://localhost:3000".parse().unwrap(),
        ]))
        .allow_headers(tower_http::cors::Any)
        .allow_methods(tower_http::cors::Any);

    // Router
    let app = Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/tunnel", get(bridge::handle_tunnel_upgrade))
        .route("/session/{id}", get(bridge::handle_viewer_upgrade))
        .route("/sessions", get(bridge::handle_list_sessions))
        .layer(cors)
        .with_state(state);

    // Start server
    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{port}"))
        .await
        .expect("failed to bind");
    tracing::info!("relay listening on 0.0.0.0:{port}");
    axum::serve(listener, app).await.unwrap();
}
