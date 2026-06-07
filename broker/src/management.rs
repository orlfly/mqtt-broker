//! In-process management channel for the MQTT broker.
//!
//! The agent's `zeroclaw` tools used to call the broker's HTTP
//! API across a `reqwest` client. That works in production,
//! but for a single-binary deployment (agent + broker in one
//! process) the HTTP hop is pure overhead — extra latency,
//! extra JSON serialisation, and a needless dependency on a
//! TCP listener plus a bearer token.
//!
//! This module replaces that path with a typed `mpsc` channel
//! + per-request `oneshot` reply, all living inside the same
//! tokio runtime. The tools hold a [`ManagementHandle`] (cheap
//! to clone) and call async methods on it; the broker's state
//! is read once on a dedicated management task. This is the
//! "线程间通讯" pattern the user asked for, applied to a real
//! read/write boundary instead of a single shared `Arc` that
//! the tools would also need to clone.
//!
//! ## Topology
//!
//! ```text
//!  agent tool ─── mpsc<ManagementRequest> ───> management task
//!       ▲                                          │
//!       └────── oneshot<Reply> ───────────────────┘
//! ```
//!
//! The management task is the *only* place that holds the
//! broker's `RwLock` for management queries. The MQTT
//! engine's own accept loop and per-connection handlers take
//! write locks when clients connect/disconnect, but the
//! management read path is separate — so a `list_clients()`
//! call from the agent can't deadlock against a `connect()`
//! on the broker side.
//!
//! ## Lifecycle
//!
//! Call [`MqttEngine::management_pair`] once at startup, clone
//! the [`ManagementHandle`] into every tool, and `tokio::spawn`
//! the returned future. When the binary exits, the `Sender`
//! is dropped and the management task drains any remaining
//! requests and exits cleanly.

use tokio::sync::{mpsc, oneshot};
use tracing::{info};

use crate::state::{ClientInfo, SharedBrokerState, Subscription};

/// One management operation. Each variant carries a `oneshot`
/// reply channel so the requester can `.await` the result
/// without holding a lock.
///
/// The `String` fields (e.g. `client_id`, `topic`) are owned
/// so the request can be sent through a channel and then
/// dropped by the management task — the original caller no
/// longer needs to keep the buffer alive.
pub enum ManagementRequest {
    ListClients(oneshot::Sender<Vec<ClientInfo>>),
    GetClient {
        client_id: String,
        reply: oneshot::Sender<Option<ClientInfo>>,
    },
    ListSubscriptions(oneshot::Sender<Vec<TopicSubscribers>>),
    GetTopicSubscribers {
        topic: String,
        reply: oneshot::Sender<Vec<Subscription>>,
    },
}

/// A topic + its subscriber list, materialised by the
/// management task so the caller never sees the broker's
/// internal `HashMap` shape.
#[derive(Debug, Clone)]
pub struct TopicSubscribers {
    pub topic: String,
    pub subscribers: Vec<Subscription>,
}

/// Cheap, cloneable handle to the management task. The
/// agent's tools hold one of these and call async methods
/// on it; the actual state access happens on a dedicated
/// tokio task.
///
/// Cloning is cheap: the inner `mpsc::Sender` is a single
/// `Arc` over the channel state.
#[derive(Clone)]
pub struct ManagementHandle {
    tx: mpsc::Sender<ManagementRequest>,
}

impl ManagementHandle {
    /// List every currently-connected client.
    pub async fn list_clients(&self) -> Vec<ClientInfo> {
        let (tx, rx) = oneshot::channel();
        // The only way `send` fails is if the receiver is
        // dropped, which only happens when the management
        // task has exited — i.e. the process is shutting
        // down. Surface that as a panic rather than
        // silently returning an empty list, because the
        // alternative (silently returning `[]` on a
        // half-dead broker) would be a footgun for the
        // agent.
        self.tx
            .send(ManagementRequest::ListClients(tx))
            .await
            .expect("management task is not running");
        rx.await
            .expect("management task dropped the reply sender")
    }

    /// Look up a single client by id.
    pub async fn get_client(&self, client_id: &str) -> Option<ClientInfo> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(ManagementRequest::GetClient {
                client_id: client_id.to_string(),
                reply: tx,
            })
            .await
            .expect("management task is not running");
        rx.await
            .expect("management task dropped the reply sender")
    }

    /// List every subscribed topic along with the
    /// subscribers for each. Equivalent to a snapshot of
    /// the broker's subscription table.
    pub async fn list_subscriptions(&self) -> Vec<TopicSubscribers> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(ManagementRequest::ListSubscriptions(tx))
            .await
            .expect("management task is not running");
        rx.await
            .expect("management task dropped the reply sender")
    }

    /// List the subscribers of a single topic. Returns an
    /// empty `Vec` if nobody is subscribed.
    pub async fn get_topic_subscribers(&self, topic: &str) -> Vec<Subscription> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(ManagementRequest::GetTopicSubscribers {
                topic: topic.to_string(),
                reply: tx,
            })
            .await
            .expect("management task is not running");
        rx.await
            .expect("management task dropped the reply sender")
    }
}

/// Channel buffer size for the management mpsc. At this
/// scale every request is handled in microseconds, so 32
/// slots of buffering is more than enough. `mpsc::Sender::send`
/// is back-pressured, so the tools will naturally wait if
/// the buffer fills.
pub const MGMT_CHANNEL_DEPTH: usize = 32;

/// Drive the management channel: read a request, take the
/// broker's read lock, build the reply, send it back. This
/// task is the **only** place that holds the read lock for
/// management queries.
///
/// The lock is held only for the duration of the clone+map
/// that builds the reply, not for the duration of the
/// `oneshot::send` (which is just a pointer copy). That
/// keeps the critical section short even if the request
/// payload is large.
///
/// Exits when the channel is closed (i.e. the last
/// `ManagementHandle` was dropped). The exit log line is
/// useful in shutdown traces — it tells the operator that
/// the management task did terminate, not that it was
/// killed by a panic.
pub async fn management_loop(
    state: SharedBrokerState,
    mut rx: mpsc::Receiver<ManagementRequest>,
) {
    info!("[broker management] task started");
    while let Some(req) = rx.recv().await {
        // Hold the read lock only across the clone/collect
        // that builds the reply payload. The `oneshot::send`
        // below runs lock-free.
        let state_guard = state.read().await;
        match req {
            ManagementRequest::ListClients(reply) => {
                let clients: Vec<ClientInfo> =
                    state_guard.clients.values().cloned().collect();
                drop(state_guard);
                // Log-and-ignore the send error: it only
                // happens if the caller was dropped (e.g.
                // the LLM cancelled the tool call), which
                // is fine.
                if reply.send(clients).is_err() {
                    tracing::debug!("[broker management] list_clients caller dropped");
                }
            }
            ManagementRequest::GetClient { client_id, reply } => {
                let client = state_guard.clients.get(&client_id).cloned();
                drop(state_guard);
                if reply.send(client).is_err() {
                    tracing::debug!(
                        "[broker management] get_client({client_id}) caller dropped"
                    );
                }
            }
            ManagementRequest::ListSubscriptions(reply) => {
                let topics: Vec<TopicSubscribers> = state_guard
                    .subscriptions
                    .iter()
                    .map(|(topic, subs)| TopicSubscribers {
                        topic: topic.clone(),
                        subscribers: subs.clone(),
                    })
                    .collect();
                drop(state_guard);
                if reply.send(topics).is_err() {
                    tracing::debug!("[broker management] list_subscriptions caller dropped");
                }
            }
            ManagementRequest::GetTopicSubscribers { topic, reply } => {
                let subs: Vec<Subscription> = state_guard
                    .subscriptions
                    .get(&topic)
                    .cloned()
                    .unwrap_or_default();
                drop(state_guard);
                if reply.send(subs).is_err() {
                    tracing::debug!(
                        "[broker management] get_topic_subscribers({topic}) caller dropped"
                    );
                }
            }
        }
    }
    info!("[broker management] task exiting (channel closed)");
}

/// Build the (handle, future) pair. The caller is responsible
/// for spawning the future on the tokio runtime.
pub fn management_pair(
    state: SharedBrokerState,
) -> (ManagementHandle, impl std::future::Future<Output = ()> + Send) {
    let (tx, rx) = mpsc::channel(MGMT_CHANNEL_DEPTH);
    let handle = ManagementHandle { tx };
    let task = management_loop(state, rx);
    (handle, task)
}
