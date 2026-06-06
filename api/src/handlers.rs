use std::sync::Arc;
use axum::{extract::State, Json, extract::Path};
use broker::SharedBrokerState;

use crate::models::{
    ApiResponse, ClientInfoResponse, SubscriptionInfoResponse,
    SubscriberInfo, AuthRequest, AuthResponse, HealthResponse,
};
use crate::auth::JwtAuth;

#[derive(Clone)]
pub struct AppState {
    pub broker_state: SharedBrokerState,
    pub jwt_auth: Arc<JwtAuth>,
    pub startup_time: std::time::Instant,
}

pub async fn health_check(
    State(state): State<AppState>,
) -> Json<ApiResponse<HealthResponse>> {
    let broker = state.broker_state.read().await;
    let connection_count = broker.clients.len();

    let response = HealthResponse {
        status: "ok".to_string(),
        uptime_secs: state.startup_time.elapsed().as_secs(),
        components: serde_json::json!({
            "mqtt": {
                "status": "ok",
                "connections": connection_count,
            },
            "api": {
                "status": "ok",
            },
        }),
    };

    Json(ApiResponse::ok(response))
}

pub async fn list_clients(
    State(state): State<AppState>,
) -> Json<ApiResponse<Vec<ClientInfoResponse>>> {
    let broker = state.broker_state.read().await;
    let clients: Vec<ClientInfoResponse> = broker
        .list_clients()
        .into_iter()
        .map(ClientInfoResponse::from)
        .collect();

    Json(ApiResponse::ok(clients))
}

pub async fn get_client(
    State(state): State<AppState>,
    Path(client_id): Path<String>,
) -> Json<ApiResponse<ClientInfoResponse>> {
    let broker = state.broker_state.read().await;
    match broker.get_client(&client_id) {
        Some(client) => Json(ApiResponse::ok(ClientInfoResponse::from(client))),
        None => Json(ApiResponse::err("NOT_FOUND", format!("Client {} not found", client_id))),
    }
}

pub async fn list_subscriptions(
    State(state): State<AppState>,
) -> Json<ApiResponse<Vec<SubscriptionInfoResponse>>> {
    let broker = state.broker_state.read().await;
    let topics: Vec<SubscriptionInfoResponse> = broker
        .list_topics()
        .into_iter()
        .map(|topic| {
            let subscribers = broker.get_topic_subscribers(topic);
            SubscriptionInfoResponse {
                topic: topic.to_string(),
                subscribers: subscribers
                    .into_iter()
                    .map(|s| SubscriberInfo {
                        client_id: s.client_id.clone(),
                        qos: format!("{:?}", s.qos),
                    })
                    .collect(),
            }
        })
        .collect();

    Json(ApiResponse::ok(topics))
}

pub async fn get_topic_subscribers(
    State(state): State<AppState>,
    Path(topic): Path<String>,
) -> Json<ApiResponse<SubscriptionInfoResponse>> {
    let broker = state.broker_state.read().await;
    let subscribers = broker.get_topic_subscribers(&topic);

    if subscribers.is_empty() {
        return Json(ApiResponse::err("NOT_FOUND", format!("No subscribers for topic {}", topic)));
    }

    let response = SubscriptionInfoResponse {
        topic,
        subscribers: subscribers
            .into_iter()
            .map(|s| SubscriberInfo {
                client_id: s.client_id.clone(),
                qos: format!("{:?}", s.qos),
            })
            .collect(),
    };

    Json(ApiResponse::ok(response))
}

pub async fn auth_token(
    State(state): State<AppState>,
    Json(req): Json<AuthRequest>,
) -> Json<ApiResponse<AuthResponse>> {
    if req.username == "admin" && req.password == "admin" {
        match state.jwt_auth.generate_token(&req.username, "admin") {
            Ok(token) => Json(ApiResponse::ok(AuthResponse {
                token,
                expires_in: state.jwt_auth.expire_secs(),
            })),
            Err(_) => Json(ApiResponse::err("TOKEN_ERROR", "Failed to generate token")),
        }
    } else {
        Json(ApiResponse::err("UNAUTHORIZED", "Invalid username or password"))
    }
}

pub async fn refresh_token(
    State(state): State<AppState>,
    Json(req): Json<AuthRequest>,
) -> Json<ApiResponse<AuthResponse>> {
    auth_token(State(state), Json(req)).await
}
