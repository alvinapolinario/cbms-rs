mod auth;
mod config;
mod db;
mod error;
mod extract;
mod models;
mod money;
mod rows;
mod routes;
mod services;
mod types;

use axum::{routing::get, Json, Router};
use serde_json::json;
use std::sync::Arc;

pub struct AppState {
    pub db: db::DbPool,
    pub config: config::Config,
}

async fn health(axum::extract::State(state): axum::extract::State<Arc<AppState>>) -> Json<serde_json::Value> {
    let db_ok = sqlx::query("SELECT 1").execute(&state.db).await.is_ok();
    Json(json!({
        "ok": db_ok,
        "status": if db_ok { "healthy" } else { "unhealthy" },
        "database": if db_ok { "connected" } else { "disconnected" },
    }))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();
    tracing_subscriber::fmt::init();

    let config = config::Config::from_env();
    let db = db::connect(&config.database_url).await?;
    tracing::info!("connected to database");

    let addr = format!("{}:{}", config.host, config.port);
    let state = Arc::new(AppState { db, config });

    // Laravel's RouteServiceProvider prefixes every routes/api.php route with `/api`
    // (`->prefix('api')`) -- every real client (mobile/desktop apps) calls e.g.
    // `/api/login-app`, not `/login-app`. Matched here so those clients work unmodified.
    let app = Router::new()
        .route("/health", get(health))
        .nest("/api", routes::router())
        .with_state(state);

    tracing::info!("sabonglara-rs listening on {addr}");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
