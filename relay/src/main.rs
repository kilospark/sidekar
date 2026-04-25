mod auth;
mod bridge;
mod registry;
mod slack;
mod telegram;
mod types;

use axum::{routing::{get, post}, Router};
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

    // Required env vars. Trim both — secret stores sometimes attach
    // trailing whitespace or a newline that breaks URI parsing.
    let mongodb_uri = std::env::var("MONGODB_URI")
        .expect("MONGODB_URI environment variable is required")
        .trim()
        .to_string();
    if !(mongodb_uri.starts_with("mongodb://") || mongodb_uri.starts_with("mongodb+srv://")) {
        // Fail loudly with a hint instead of the opaque "no scheme"
        // error from the driver.
        panic!(
            "MONGODB_URI must start with mongodb:// or mongodb+srv:// (got {} chars, first 12: {:?})",
            mongodb_uri.len(),
            mongodb_uri.chars().take(12).collect::<String>()
        );
    }
    let jwt_secret = std::env::var("JWT_SECRET")
        .expect("JWT_SECRET environment variable is required")
        .trim()
        .to_string();
    let port = std::env::var("PORT").unwrap_or_else(|_| "8080".into());
    let instance_id = std::env::var("RELAY_INSTANCE_ID")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| uuid::Uuid::new_v4().to_string());
    let public_origin = std::env::var("RELAY_PUBLIC_ORIGIN")
        .unwrap_or_else(|_| "https://relay.sidekar.dev".into())
        .trim_end_matches('/')
        .to_string();

    // Connect to MongoDB
    let client = mongodb::Client::with_uri_str(&mongodb_uri)
        .await
        .expect("failed to connect to MongoDB");
    let db = client.database("sidekar");
    tracing::info!("connected to MongoDB");

    // Create registry (hybrid: MongoDB for metadata, in-memory for live connections)
    let registry = Registry::new(db.clone(), instance_id, public_origin);
    registry.start_heartbeat();
    registry.start_bus_dispatcher();

    // Optional Telegram integration.
    let telegram = match telegram::TelegramConfig::from_env() {
        Some(cfg) => {
            tracing::info!("telegram integration enabled");
            // Ensure unique index on update_id so dedup relies on the
            // insert-or-duplicate-key contract.
            if let Err(e) = telegram::ensure_indexes(&db).await {
                tracing::warn!("telegram index setup failed (dedup may be soft): {e}");
            }
            Some(telegram::TelegramState::new(cfg))
        }
        None => {
            tracing::info!(
                "telegram integration disabled (TELEGRAM_BOT_TOKEN / TELEGRAM_WEBHOOK_SECRET unset)"
            );
            None
        }
    };

    // Optional Slack integration.
    let slack = match slack::SlackConfig::from_env() {
        Some(cfg) => {
            tracing::info!("slack integration enabled");
            if let Err(e) = slack::ensure_indexes(&db).await {
                tracing::warn!("slack index setup failed (dedup may be soft): {e}");
            }
            let state = slack::SlackState::new(cfg);
            // Resolve bot user ID in background so self-message filtering
            // works from the first event.
            state.resolve_bot_user_id().await;
            Some(state)
        }
        None => {
            tracing::info!(
                "slack integration disabled (SLACK_BOT_TOKEN / SLACK_SIGNING_SECRET unset)"
            );
            None
        }
    };

    // App state
    let state = AppState {
        db,
        registry,
        jwt_secret,
        telegram,
        slack,
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
        .route("/session/{id}/resolve", get(bridge::handle_resolve_session))
        .route("/sessions", get(bridge::handle_list_sessions))
        .route("/relay/bus", post(bridge::handle_relay_bus))
        .route("/telegram/webhook", post(telegram::handle_webhook))
        .route("/telegram/deliver", post(telegram::handle_deliver))
        .route("/telegram/link", get(telegram::handle_mint_link_code))
        .route("/slack/events", post(slack::handle_events))
        .route("/slack/commands", post(slack::handle_slash_command))
        .route("/slack/deliver", post(slack::handle_deliver))
        .route("/slack/link", get(slack::handle_mint_link_code))
        .layer(cors)
        .with_state(state);

    // Start server
    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{port}"))
        .await
        .expect("failed to bind");
    tracing::info!("relay listening on 0.0.0.0:{port}");
    axum::serve(listener, app).await.unwrap();
}
