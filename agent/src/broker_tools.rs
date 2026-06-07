//! `zeroclaw` `Tool` implementations that wrap the broker's
//! in-process management channel.
//!
//! ## Why not HTTP?
//!
//! Earlier versions of these tools used `reqwest` to call the
//! broker's axum-based HTTP API. That worked in production,
//! but the broker and the agent were eventually merged into a
//! single binary (see the `app` crate), and the HTTP hop
//! became pure overhead:
//!
//! - extra latency for every tool call
//! - extra JSON (de)serialisation on both sides
//! - a needless dependency on a TCP listener + bearer token
//!   inside the same process
//!
//! The new path is a typed `mpsc` channel
//! (`broker::management::ManagementHandle`) with per-request
//! `oneshot` replies. The tools hold a clone of the handle
//! and call async methods; the broker's state is read on a
//! dedicated management task. See `broker::management` for
//! the full topology.
//!
//! The HTTP API itself is still served by the merged binary
//! (for external admin tools and scripts), but these tools
//! deliberately don't touch it.

use async_trait::async_trait;
use broker::management::ManagementHandle;
use serde_json::{json, Value};
use zeroclaw::tools::{Tool, ToolResult};

// ----- list_clients -----------------------------------------------------

pub struct ListClientsTool {
    mgmt: ManagementHandle,
}

impl ListClientsTool {
    pub fn new(mgmt: ManagementHandle) -> Self {
        Self { mgmt }
    }
}

#[async_trait]
impl Tool for ListClientsTool {
    fn name(&self) -> &str {
        "list_clients"
    }

    fn description(&self) -> &str {
        "列出所有当前连接的 MQTT 客户端"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {},
            "required": []
        })
    }

    async fn execute(&self, _args: Value) -> anyhow::Result<ToolResult> {
        let clients = self.mgmt.list_clients().await;
        // Render as a compact, LLM-friendly JSON array.
        // `ClientInfo`'s `Instant` connected_at is serialised
        // as "elapsed since now" so the LLM gets a relative
        // "connected for 12s" feel — the LLM has no use for
        // a wall-clock timestamp, and absolute times can be
        // misleading in a long-lived process.
        let body: Vec<Value> = clients
            .into_iter()
            .map(|c| {
                json!({
                    "client_id": c.client_id,
                    "address": c.addr.to_string(),
                    "protocol": format!("{:?}", c.protocol_version),
                    "connected_for_secs": c.connected_at.elapsed().as_secs(),
                    "username": c.username,
                    "keep_alive": c.keep_alive,
                    "clean_session": c.clean_session,
                })
            })
            .collect();
        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&body).unwrap_or_default(),
            error: None,
        })
    }
}

// ----- list_topics ------------------------------------------------------

pub struct ListTopicsTool {
    mgmt: ManagementHandle,
}

impl ListTopicsTool {
    pub fn new(mgmt: ManagementHandle) -> Self {
        Self { mgmt }
    }
}

#[async_trait]
impl Tool for ListTopicsTool {
    fn name(&self) -> &str {
        "list_topics"
    }

    fn description(&self) -> &str {
        "列出所有被订阅的 Topic 及其订阅者"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {},
            "required": []
        })
    }

    async fn execute(&self, _args: Value) -> anyhow::Result<ToolResult> {
        let topics = self.mgmt.list_subscriptions().await;
        let body: Vec<Value> = topics
            .into_iter()
            .map(|t| {
                json!({
                    "topic": t.topic,
                    "subscriber_count": t.subscribers.len(),
                    "subscribers": t.subscribers.iter().map(|s| {
                        json!({
                            "client_id": s.client_id,
                            "qos": format!("{:?}", s.qos),
                        })
                    }).collect::<Vec<_>>(),
                })
            })
            .collect();
        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&body).unwrap_or_default(),
            error: None,
        })
    }
}

// ----- get_topic_subscribers -------------------------------------------

pub struct GetTopicSubscribersTool {
    mgmt: ManagementHandle,
}

impl GetTopicSubscribersTool {
    pub fn new(mgmt: ManagementHandle) -> Self {
        Self { mgmt }
    }
}

#[async_trait]
impl Tool for GetTopicSubscribersTool {
    fn name(&self) -> &str {
        "get_topic_subscribers"
    }

    fn description(&self) -> &str {
        "查看指定 Topic 的订阅者列表"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "topic": {
                    "type": "string",
                    "description": "Topic 名称"
                }
            },
            "required": ["topic"]
        })
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let topic = args["topic"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing topic parameter"))?;
        let subscribers = self.mgmt.get_topic_subscribers(topic).await;
        let body = json!({
            "topic": topic,
            "subscriber_count": subscribers.len(),
            "subscribers": subscribers.iter().map(|s| {
                json!({
                    "client_id": s.client_id,
                    "topic_filter": s.topic_filter,
                    "qos": format!("{:?}", s.qos),
                })
            }).collect::<Vec<_>>(),
        });
        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&body).unwrap_or_default(),
            error: None,
        })
    }
}
