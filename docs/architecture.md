# MQTT Broker AI Agent 管理系统 — 架构文档

## 系统概述

MQTT Broker AI Agent 管理系统是一个**单进程**服务（构建产物为 `app`），提供：
- 标准 MQTT 3.1.1/5.0 Broker 能力（1883/8883）
- RESTful API 管理接口（8080，可选）
- AI Agent 自然语言交互管理（带语音通道）
- 多通道接入（文本、QQ Bot、语音）

## 进程内拓扑

```
┌─────────────────── app (single process / single tokio runtime) ───────────────────┐
│                                                                                  │
│   MqttEngine ───── accept loop on 1883 ────────────────► MQTT                    │
│   state   │                                                                      │
│           └── management task (broker::management)                               │
│                    ▲                                                             │
│                    │ mpsc<ManagementRequest> + oneshot                           │
│                    │                                                             │
│   agent::broker_tools (in-process tools)                                         │
│   ┌─ ListClientsTool       ─┐                                                    │
│   ├─ ListTopicsTool        ─┤  ───► LLM (zeroclaw) ──► ASR / TTS / KWS / capture │
│   └─ GetTopicSubscribers   ─┘                                                    │
│                                                                                  │
│   HTTP API (axum) ────── listener on 8080 ────────────► external admin tools     │
│   (optional, cfg.api.enabled)                                                    │
│                                                                                  │
└──────────────────────────────────────────────────────────────────────────────────┘
```

## Crate 布局

| Crate      | 类型 | 职责 |
|------------|------|------|
| `broker`   | lib  | MQTT 引擎 + `management` 通道 |
| `api`      | lib  | axum 路由 + JWT 鉴权（外部管理用，可选启动） |
| `agent`    | lib  | voice_loop + broker_tools + followup classifier |
| `voice`    | lib  | sherpa KWS/ASR/TTS + cpal capture/playback + 设备诊断 |
| `common`   | lib  | 配置结构（`broker.yaml`） |
| `app`      | bin  | **唯一的二进制**：编排以上所有 crate |

## 关键模块

### `broker::management`（新增）
进程内管理通道，agent 工具不再走 HTTP：
- `ManagementRequest` — 请求枚举（`ListClients` / `GetClient` / `ListSubscriptions` / `GetTopicSubscribers`）
- `ManagementHandle` — 克隆代价低的句柄，工具持有
- `management_loop` — 唯一持有 broker 读锁的后台任务
- `MqttEngine::management_pair()` — 返回 `(handle, future)`，由调用者 `tokio::spawn`

### `agent::broker_tools`（从 `skills::mqtt_manager` 改名）
三个 `zeroclaw::Tool` 实现，全部基于 `ManagementHandle`：
- `ListClientsTool`
- `ListTopicsTool`
- `GetTopicSubscribersTool`

### HTTP API（保留）
- `api::create_router(broker_state, jwt_auth)` — 与 broker 共享 `SharedBrokerState`
- 启动受 `api.enabled` 控制（默认 `true`，向后兼容）
- **agent 工具不再调用 HTTP**

## 之前的架构（已废弃）

旧版有两个独立二进制：
- `server` — 跑 broker + HTTP API
- `agent`  — 跑 voice loop，通过 `reqwest` HTTP 客户端调用 `server` 的管理 API

新版的 `app` 是它们的合并。`server` crate 已删除。

## 详细设计

- 协议层：`broker` crate 各模块
- 语音管道：`voice/src/cpal_capture.rs`、`sherpa_kws.rs` 等
- 唤醒/任务流：`agent/src/voice_loop.rs`
