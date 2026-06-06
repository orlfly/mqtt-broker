use serde::{Deserialize, Serialize};
use broker::state::ClientInfo;

#[derive(Debug, Serialize)]
pub struct ApiResponse<T: Serialize> {
    pub success: bool,
    pub data: Option<T>,
    pub error: Option<ApiError>,
}

#[derive(Debug, Serialize)]
pub struct ApiError {
    pub code: String,
    pub message: String,
}

impl<T: Serialize> ApiResponse<T> {
    pub fn ok(data: T) -> Self {
        Self {
            success: true,
            data: Some(data),
            error: None,
        }
    }

    pub fn err(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            success: false,
            data: None,
            error: Some(ApiError {
                code: code.into(),
                message: message.into(),
            }),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct ClientInfoResponse {
    pub client_id: String,
    pub addr: String,
    pub protocol_version: String,
    pub connected_at_secs: u64,
    pub clean_session: bool,
    pub keep_alive: u16,
    pub username: Option<String>,
}

impl From<&ClientInfo> for ClientInfoResponse {
    fn from(c: &ClientInfo) -> Self {
        Self {
            client_id: c.client_id.clone(),
            addr: c.addr.to_string(),
            protocol_version: format!("{:?}", c.protocol_version),
            connected_at_secs: c.connected_at.elapsed().as_secs(),
            clean_session: c.clean_session,
            keep_alive: c.keep_alive,
            username: c.username.clone(),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct SubscriptionInfoResponse {
    pub topic: String,
    pub subscribers: Vec<SubscriberInfo>,
}

#[derive(Debug, Serialize)]
pub struct SubscriberInfo {
    pub client_id: String,
    pub qos: String,
}

#[derive(Debug, Deserialize)]
pub struct AuthRequest {
    pub username: String,
    pub password: String,
}

#[derive(Debug, Serialize)]
pub struct AuthResponse {
    pub token: String,
    pub expires_in: u64,
}

#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub status: String,
    pub uptime_secs: u64,
    pub components: serde_json::Value,
}
