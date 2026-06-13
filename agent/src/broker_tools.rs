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

// ----- subscribe ---------------------------------------------------------

pub struct SubscribeTool {
    mgmt: ManagementHandle,
}

impl SubscribeTool {
    pub fn new(mgmt: ManagementHandle) -> Self {
        Self { mgmt }
    }
}

#[async_trait]
impl Tool for SubscribeTool {
    fn name(&self) -> &str {
        "subscribe"
    }

    fn description(&self) -> &str {
        "订阅 MQTT Topic（回调模式）。订阅后匹配的 PUBLISH 消息会进入代理的消息队列，通过 drain_messages 获取"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "topic_filter": {
                    "type": "string",
                    "description": "Topic filter，支持 +（单层）和 #（多层）通配符。例如：sensor/# 或 device/+/temp"
                }
            },
            "required": ["topic_filter"]
        })
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let topic_filter = args["topic_filter"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing topic_filter parameter"))?;
        let result = self.mgmt.subscribe(topic_filter).await?;
        Ok(ToolResult {
            success: true,
            output: result,
            error: None,
        })
    }
}

// ----- unsubscribe -------------------------------------------------------

pub struct UnsubscribeTool {
    mgmt: ManagementHandle,
}

impl UnsubscribeTool {
    pub fn new(mgmt: ManagementHandle) -> Self {
        Self { mgmt }
    }
}

#[async_trait]
impl Tool for UnsubscribeTool {
    fn name(&self) -> &str {
        "unsubscribe"
    }

    fn description(&self) -> &str {
        "取消 MQTT Topic 订阅"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "topic_filter": {
                    "type": "string",
                    "description": "要取消订阅的 Topic filter"
                }
            },
            "required": ["topic_filter"]
        })
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let topic_filter = args["topic_filter"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing topic_filter parameter"))?;
        let result = self.mgmt.unsubscribe(topic_filter).await?;
        Ok(ToolResult {
            success: true,
            output: result,
            error: None,
        })
    }
}

// ----- publish -----------------------------------------------------------

pub struct PublishTool {
    mgmt: ManagementHandle,
}

impl PublishTool {
    pub fn new(mgmt: ManagementHandle) -> Self {
        Self { mgmt }
    }
}

#[async_trait]
impl Tool for PublishTool {
    fn name(&self) -> &str {
        "publish"
    }

    fn description(&self) -> &str {
        "向 MQTT Broker 发布消息。消息会投递给所有匹配的订阅者（包括代理自身的订阅）"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "topic": {
                    "type": "string",
                    "description": "目标 Topic"
                },
                "payload": {
                    "type": "string",
                    "description": "消息内容（UTF-8 字符串）"
                },
                "qos": {
                    "type": "integer",
                    "description": "QoS 等级（0 = 至多一次，1 = 至少一次，2 = 恰好一次）",
                    "enum": [0, 1, 2],
                    "default": 0
                },
                "retain": {
                    "type": "boolean",
                    "description": "是否保留消息",
                    "default": false
                },
                "user_properties": {
                    "type": "array",
                    "description": "MQTT v5 用户属性列表",
                    "items": {
                        "type": "object",
                        "properties": {
                            "key": { "type": "string" },
                            "value": { "type": "string" }
                        },
                        "required": ["key", "value"]
                    },
                    "default": []
                }
            },
            "required": ["topic", "payload"]
        })
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let topic = args["topic"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing topic parameter"))?;
        let payload = args["payload"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing payload parameter"))?;
        let qos = args["qos"].as_u64().unwrap_or(0) as u8;
        let retain = args["retain"].as_bool().unwrap_or(false);
        let user_properties: Vec<(String, String)> = args["user_properties"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| {
                        Some((
                            v.get("key")?.as_str()?.to_string(),
                            v.get("value")?.as_str()?.to_string(),
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let result = self
            .mgmt
            .publish(topic, payload, qos, retain, user_properties)
            .await?;
        Ok(ToolResult {
            success: true,
            output: result,
            error: None,
        })
    }
}

// ----- drain_messages ----------------------------------------------------

pub struct DrainMessagesTool {
    mgmt: ManagementHandle,
}

impl DrainMessagesTool {
    pub fn new(mgmt: ManagementHandle) -> Self {
        Self { mgmt }
    }
}

#[async_trait]
impl Tool for DrainMessagesTool {
    fn name(&self) -> &str {
        "drain_messages"
    }

    fn description(&self) -> &str {
        "读取并清空代理订阅的 MQTT 消息队列。订阅 Topic 后收到的 PUBLISH 消息会缓存于此，调用 drain_messages 一次性取出"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {},
            "required": []
        })
    }

    async fn execute(&self, _args: Value) -> anyhow::Result<ToolResult> {
        let msgs = self.mgmt.drain_messages().await;
        let body: Vec<Value> = msgs
            .into_iter()
            .map(|m| {
                let payload_str = String::from_utf8_lossy(&m.payload).to_string();
                json!({
                    "topic": m.topic,
                    "payload": payload_str,
                    "qos": format!("{:?}", m.qos),
                    "retain": m.retain,
                    "user_properties": m.user_properties.iter().map(|up| {
                        json!({"key": up.key, "value": up.value})
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
