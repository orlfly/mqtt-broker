# TOOLS

> 当前可用的 tool 清单 + 调用约定。tool schema 也会由 zeroclaw
> 自动注入到 prompt 里；这份文件是"何时用哪个"的策略补充。

## list_clients

- **何时用**：用户问"现在连着谁"、"有多少客户端"、"X 是不是连着"。
- **返回字段**：`client_id` / `address` / `protocol` /
  `connected_for_secs` / `username` / `keep_alive` / `clean_session`。
- **注意**：`connected_for_secs` 是相对时间（自连接起的秒数），
  不是绝对时间戳 —— 这是 `agent/src/broker_tools.rs` 故意做的，
  因为 LLM 没有绝对时钟。
- **空结果**：明确说"目前没有客户端连接"，**不要**说"查询失败"。

## list_topics

- **何时用**：用户问"哪些 topic 被订阅"、"有没有人监听 X"。
- **返回字段**：每个 topic 的 `subscriber_count` 和订阅者列表
  （含 `client_id` / `qos`）。
- **结合用**：拿到 topic 列表后，若用户对某个 topic 感兴趣，
  二次调 `get_topic_subscribers` 取详情（避免一次返回过大）。

## get_topic_subscribers

- **何时用**：用户指定了具体 topic ——"查 sensors/+/temp 的订阅者"。
- **参数**：`topic: string` 必填。
- **通配符**：这是 MQTT 订阅侧的通配符，不是查询语法。
  用户说"sensors/+/temp"，传的就是这个字符串。

## 不要做的事

- 不要用这些 tool 调任何 HTTP / TCP —— 它们走的是进程内
  `mpsc + oneshot` 通道（见 `broker/src/management.rs`），
  走 HTTP 反而绕远且会失败。
- 不要尝试订阅 MQTT topic 本身来"主动观察" —— 当前 tool 集
  只支持查询侧（snapshot），不支持订阅侧。

## 语音通道下的输出

`agent/src/voice_text.rs` 会在送进 TTS 之前对你的回复做一道
"语音改写" —— 去掉 `**` `##` `- ` `[](url)`、反引号、表格、emoji，
bullet list 展开成"第一，...。第二，...。"，最后按 `max_chars`
硬截断并加 `truncate_suffix`。

**含义**：

- 你**不需要**为了"听着舒服"而刻意改写自己的风格，照常用
  markdown 写 —— 改写由 `voice_text` 负责。
- 但也意味着：markdown 之外的富格式（如 `> 引用`、HTML 标签）
  **不会**被改写，念出来会很怪。如果你预期 TTS 会念这段，请
  避免使用这些格式。
- 数字 / 英文保留原样（`1883` 不会被翻译成汉字），由 Piper 自己
  决定怎么读；不要为了"配合" TTS 而提前写成汉字。
- `max_chars` 是硬截断。**写长回复前先压缩**；如果一定需要
  长回复（例如工具返回了一大段结果），考虑分多次回答。
