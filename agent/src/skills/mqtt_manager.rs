use async_trait::async_trait;
use serde_json::{json, Value};
use zeroclaw::tools::{Tool, ToolResult};

pub struct ListClientsTool {
    api_base_url: String,
    api_token: String,
    client: reqwest::Client,
}

impl ListClientsTool {
    pub fn new(api_base_url: String, api_token: String) -> Self {
        Self {
            api_base_url,
            api_token,
            client: reqwest::Client::new(),
        }
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
        let resp = self
            .client
            .get(format!("{}/api/clients", self.api_base_url))
            .header("Authorization", format!("Bearer {}", self.api_token))
            .send()
            .await?;

        let body: Value = resp.json().await?;
        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&body).unwrap_or_default(),
            error: None,
        })
    }
}

pub struct ListTopicsTool {
    api_base_url: String,
    api_token: String,
    client: reqwest::Client,
}

impl ListTopicsTool {
    pub fn new(api_base_url: String, api_token: String) -> Self {
        Self {
            api_base_url,
            api_token,
            client: reqwest::Client::new(),
        }
    }
}

#[async_trait]
impl Tool for ListTopicsTool {
    fn name(&self) -> &str {
        "list_topics"
    }

    fn description(&self) -> &str {
        "列出所有被订阅的 Topic"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {},
            "required": []
        })
    }

    async fn execute(&self, _args: Value) -> anyhow::Result<ToolResult> {
        let resp = self
            .client
            .get(format!("{}/api/subscriptions", self.api_base_url))
            .header("Authorization", format!("Bearer {}", self.api_token))
            .send()
            .await?;

        let body: Value = resp.json().await?;
        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&body).unwrap_or_default(),
            error: None,
        })
    }
}

pub struct GetTopicSubscribersTool {
    api_base_url: String,
    api_token: String,
    client: reqwest::Client,
}

impl GetTopicSubscribersTool {
    pub fn new(api_base_url: String, api_token: String) -> Self {
        Self {
            api_base_url,
            api_token,
            client: reqwest::Client::new(),
        }
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

        let resp = self
            .client
            .get(format!("{}/api/subscriptions/{}", self.api_base_url, topic))
            .header("Authorization", format!("Bearer {}", self.api_token))
            .send()
            .await?;

        let body: Value = resp.json().await?;
        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&body).unwrap_or_default(),
            error: None,
        })
    }
}
