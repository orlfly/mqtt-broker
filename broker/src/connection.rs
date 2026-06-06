use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;
use tokio::net::TcpStream;
use tracing::info;

use crate::state::{
    ClientInfo, MqttProtocol,
    SharedBrokerState, PublishPacket,
};
use crate::session::SessionManager;
use crate::subscription::SubscriptionTree;
use crate::auth::AuthProvider;

pub struct ConnectionHandler {
    state: SharedBrokerState,
    session_manager: Arc<SessionManager>,
    subscription_tree: Arc<SubscriptionTree>,
    #[allow(dead_code)]
    auth_provider: Arc<dyn AuthProvider>,
}

impl ConnectionHandler {
    pub fn new(
        state: SharedBrokerState,
        session_manager: Arc<SessionManager>,
        subscription_tree: Arc<SubscriptionTree>,
        auth_provider: Arc<dyn AuthProvider>,
    ) -> Self {
        Self {
            state,
            session_manager,
            subscription_tree,
            auth_provider,
        }
    }

    pub async fn handle_connection(&self, _stream: TcpStream, addr: SocketAddr) {
        info!("New connection from {}", addr);

        let client_id = format!("client_{}", addr.port());

        let client_info = ClientInfo {
            client_id: client_id.clone(),
            addr,
            protocol_version: MqttProtocol::V311,
            connected_at: Instant::now(),
            clean_session: true,
            keep_alive: 60,
            username: None,
        };

        {
            let mut state = self.state.write().await;
            state.clients.insert(client_id.clone(), client_info);
        }

        self.session_manager.create_session(&client_id, true, None).await;

        loop {
            tokio::time::sleep(tokio::time::Duration::from_secs(30)).await;

            let still_connected = {
                let state = self.state.read().await;
                state.clients.contains_key(&client_id)
            };

            if !still_connected {
                break;
            }
        }

        info!("Connection closed: {}", addr);
    }

    pub async fn disconnect_client(&self, client_id: &str) {
        let will = {
            let state = self.state.read().await;
            state.session_store.get(client_id)
                .and_then(|s| s.will_message.clone())
        };

        self.subscription_tree.remove_client_subscriptions(client_id).await;

        if let Some(will) = will {
            let packet = PublishPacket {
                topic: will.topic,
                payload: will.payload,
                qos: will.qos,
                retain: will.retain,
            };

            let subscribers = self.subscription_tree.match_topic(&packet.topic).await;
            for sub in subscribers {
                info!("Delivering will message to {}", sub.client_id);
            }
        }

        let _ = self.session_manager.delete_session(client_id).await;

        {
            let mut state = self.state.write().await;
            state.clients.remove(client_id);
        }

        info!("Client {} disconnected", client_id);
    }
}
