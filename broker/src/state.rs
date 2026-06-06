use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::RwLock;

#[derive(Debug, Clone, PartialEq)]
pub enum MqttProtocol {
    V311,
    V500,
}

#[derive(Debug, Clone, PartialEq)]
pub enum QoS {
    AtMostOnce,
    AtLeastOnce,
    ExactlyOnce,
}

#[derive(Debug, Clone)]
pub struct ClientInfo {
    pub client_id: String,
    pub addr: SocketAddr,
    pub protocol_version: MqttProtocol,
    pub connected_at: Instant,
    pub clean_session: bool,
    pub keep_alive: u16,
    pub username: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Subscription {
    pub client_id: String,
    pub topic_filter: String,
    pub qos: QoS,
}

#[derive(Debug, Clone)]
pub struct SessionState {
    pub client_id: String,
    pub subscriptions: Vec<Subscription>,
    pub pending_messages: Vec<PublishPacket>,
    pub will_message: Option<WillMessage>,
}

#[derive(Debug, Clone)]
pub struct PublishPacket {
    pub topic: String,
    pub payload: Vec<u8>,
    pub qos: QoS,
    pub retain: bool,
}

#[derive(Debug, Clone)]
pub struct WillMessage {
    pub topic: String,
    pub payload: Vec<u8>,
    pub qos: QoS,
    pub retain: bool,
}

#[derive(Debug)]
pub struct BrokerState {
    pub clients: HashMap<String, ClientInfo>,
    pub subscriptions: HashMap<String, Vec<Subscription>>,
    pub session_store: HashMap<String, SessionState>,
}

impl BrokerState {
    pub fn new() -> Self {
        Self {
            clients: HashMap::new(),
            subscriptions: HashMap::new(),
            session_store: HashMap::new(),
        }
    }

    pub fn list_clients(&self) -> Vec<&ClientInfo> {
        self.clients.values().collect()
    }

    pub fn get_client(&self, client_id: &str) -> Option<&ClientInfo> {
        self.clients.get(client_id)
    }

    pub fn list_topics(&self) -> Vec<&str> {
        self.subscriptions.keys().map(|s| s.as_str()).collect()
    }

    pub fn get_topic_subscribers(&self, topic: &str) -> Vec<&Subscription> {
        self.subscriptions
            .get(topic)
            .map(|subs| subs.iter().collect())
            .unwrap_or_default()
    }
}

pub type SharedBrokerState = Arc<RwLock<BrokerState>>;

pub fn create_shared_state() -> SharedBrokerState {
    Arc::new(RwLock::new(BrokerState::new()))
}
