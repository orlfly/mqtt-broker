use crate::state::{SessionState, Subscription, PublishPacket, WillMessage, SharedBrokerState, QoS};

pub struct SessionManager {
    state: SharedBrokerState,
}

impl SessionManager {
    pub fn new(state: SharedBrokerState) -> Self {
        Self { state }
    }

    pub async fn create_session(
        &self,
        client_id: &str,
        clean_session: bool,
        will: Option<WillMessage>,
    ) {
        let mut store = self.state.write().await;
        if clean_session {
            store.session_store.remove(client_id);
        }
        store.session_store.entry(client_id.to_string()).or_insert_with(|| SessionState {
            client_id: client_id.to_string(),
            subscriptions: Vec::new(),
            pending_messages: Vec::new(),
            will_message: will,
        });
    }

    pub async fn get_session(&self, client_id: &str) -> Option<SessionState> {
        let store = self.state.read().await;
        store.session_store.get(client_id).cloned()
    }

    pub async fn add_subscription(&self, client_id: &str, topic_filter: String, qos: QoS) {
        let mut store = self.state.write().await;
        if let Some(session) = store.session_store.get_mut(client_id) {
            session.subscriptions.retain(|s| s.topic_filter != topic_filter);
            session.subscriptions.push(Subscription {
                client_id: client_id.to_string(),
                topic_filter,
                qos,
                no_local: false,
                retain_as_published: false,
                retain_handling: 0,
            });
        }
    }

    pub async fn remove_subscription(&self, client_id: &str, topic_filter: &str) {
        let mut store = self.state.write().await;
        if let Some(session) = store.session_store.get_mut(client_id) {
            session.subscriptions.retain(|s| s.topic_filter != topic_filter);
        }
    }

    pub async fn enqueue_pending(&self, client_id: &str, packet: PublishPacket) {
        let mut store = self.state.write().await;
        if let Some(session) = store.session_store.get_mut(client_id) {
            session.pending_messages.push(packet);
        }
    }

    pub async fn drain_pending(&self, client_id: &str) -> Vec<PublishPacket> {
        let mut store = self.state.write().await;
        if let Some(session) = store.session_store.get_mut(client_id) {
            session.pending_messages.drain(..).collect()
        } else {
            Vec::new()
        }
    }

    pub async fn delete_session(&self, client_id: &str) -> Option<SessionState> {
        let mut store = self.state.write().await;
        store.session_store.remove(client_id)
    }
}
