use std::sync::Arc;
use axum::{
    Router,
    routing::{get, post},
};
use broker::SharedBrokerState;

use crate::handlers::{self, AppState};
use crate::auth::JwtAuth;

pub fn create_router(
    broker_state: SharedBrokerState,
    jwt_auth: Arc<JwtAuth>,
) -> Router {
    let state = AppState {
        broker_state,
        jwt_auth: jwt_auth.clone(),
        startup_time: std::time::Instant::now(),
    };

    Router::new()
        .route("/health", get(handlers::health_check))
        .route("/api/auth/token", post(handlers::auth_token))
        .route("/api/auth/token/refresh", post(handlers::refresh_token))
        .route("/api/clients", get(handlers::list_clients))
        .route("/api/clients/{client_id}", get(handlers::get_client))
        .route("/api/subscriptions", get(handlers::list_subscriptions))
        .route("/api/subscriptions/{topic}", get(handlers::get_topic_subscribers))
        .with_state(state)
}
