# MQTT Broker AI Agent 管理系统 — 架构文档

## 系统概述

MQTT Broker AI Agent 管理系统是一个系统级服务，提供：
- 标准 MQTT 3.1.1/5.0 Broker 能力
- RESTful API 管理接口
- AI Agent 自然语言交互管理
- 多通道接入（文本、QQ Bot、语音）

## 架构层次

```
Channel 层 → Agent 层 → API 层 → Broker 层
```

## 模块职责

- **broker**: MQTT 协议引擎、会话管理、订阅树、认证、持久化
- **api**: RESTful 管理接口、Token 鉴权、请求日志
- **agent**: AI Agent 核心、LLM 客户端、Skills、Channels
- **voice**: ASR/TTS 封装、音频 I/O

详细设计请参考架构设计文档。
