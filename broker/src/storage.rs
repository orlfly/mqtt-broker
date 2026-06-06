use crate::state::{SessionState, PublishPacket};

#[async_trait::async_trait]
pub trait StorageEngine: Send + Sync {
    async fn save_session(&self, client_id: &str, session: &SessionState) -> anyhow::Result<()>;
    async fn load_session(&self, client_id: &str) -> anyhow::Result<Option<SessionState>>;
    async fn delete_session(&self, client_id: &str) -> anyhow::Result<()>;
    async fn save_pending(&self, client_id: &str, messages: &[PublishPacket]) -> anyhow::Result<()>;
    async fn load_pending(&self, client_id: &str) -> anyhow::Result<Vec<PublishPacket>>;
    async fn clear_pending(&self, client_id: &str) -> anyhow::Result<()>;
}

pub struct MemoryStorage;

#[async_trait::async_trait]
impl StorageEngine for MemoryStorage {
    async fn save_session(&self, _client_id: &str, _session: &SessionState) -> anyhow::Result<()> {
        Ok(())
    }

    async fn load_session(&self, _client_id: &str) -> anyhow::Result<Option<SessionState>> {
        Ok(None)
    }

    async fn delete_session(&self, _client_id: &str) -> anyhow::Result<()> {
        Ok(())
    }

    async fn save_pending(&self, _client_id: &str, _messages: &[PublishPacket]) -> anyhow::Result<()> {
        Ok(())
    }

    async fn load_pending(&self, _client_id: &str) -> anyhow::Result<Vec<PublishPacket>> {
        Ok(Vec::new())
    }

    async fn clear_pending(&self, _client_id: &str) -> anyhow::Result<()> {
        Ok(())
    }
}
