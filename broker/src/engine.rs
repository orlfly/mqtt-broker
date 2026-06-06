use std::sync::Arc;
use tokio::net::TcpListener;
use tracing::{info, error};

use crate::state::{SharedBrokerState, create_shared_state};
use crate::connection::ConnectionHandler;
use crate::session::SessionManager;
use crate::subscription::SubscriptionTree;
use crate::auth::{AuthProvider, AllowAllAuth};

pub struct MqttEngine {
    state: SharedBrokerState,
    session_manager: Arc<SessionManager>,
    subscription_tree: Arc<SubscriptionTree>,
    connection_handler: Arc<ConnectionHandler>,
}

impl MqttEngine {
    pub fn new() -> Self {
        let state = create_shared_state();
        let session_manager = Arc::new(SessionManager::new(state.clone()));
        let subscription_tree = Arc::new(SubscriptionTree::new(state.clone()));
        let auth_provider: Arc<dyn AuthProvider> = Arc::new(AllowAllAuth);

        let connection_handler = Arc::new(ConnectionHandler::new(
            state.clone(),
            session_manager.clone(),
            subscription_tree.clone(),
            auth_provider,
        ));

        Self {
            state,
            session_manager,
            subscription_tree,
            connection_handler,
        }
    }

    pub fn state(&self) -> SharedBrokerState {
        self.state.clone()
    }

    pub fn session_manager(&self) -> Arc<SessionManager> {
        self.session_manager.clone()
    }

    pub fn subscription_tree(&self) -> Arc<SubscriptionTree> {
        self.subscription_tree.clone()
    }

    pub async fn start(&self, bind_addr: &str) -> anyhow::Result<()> {
        let listener = TcpListener::bind(bind_addr).await?;
        info!("MQTT Broker listening on {}", bind_addr);

        loop {
            match listener.accept().await {
                Ok((stream, addr)) => {
                    let handler = self.connection_handler.clone();
                    tokio::spawn(async move {
                        handler.handle_connection(stream, addr).await;
                    });
                }
                Err(e) => {
                    error!("Failed to accept connection: {}", e);
                }
            }
        }
    }
}

impl Default for MqttEngine {
    fn default() -> Self {
        Self::new()
    }
}
