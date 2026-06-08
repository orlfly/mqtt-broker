use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

pub const DEFAULT_CONFIG_PATH: &str = "config/broker.yaml";
pub const CONFIG_PATH_ENV: &str = "BROKER_CONFIG";

#[derive(Debug, Deserialize, Clone)]
pub struct Config {
    pub broker: BrokerConfig,
    pub api: ApiConfig,
    pub agent: AgentConfig,
}

#[derive(Debug, Deserialize, Clone)]
pub struct BrokerConfig {
    pub host: String,
    pub mqtt_port: u16,
    #[serde(default)]
    pub mqtts_port: Option<u16>,
    #[serde(default)]
    pub max_connections: Option<u32>,
    #[serde(default)]
    pub auth: Option<BrokerAuthConfig>,
    #[serde(default)]
    pub tls: Option<BrokerTlsConfig>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct BrokerAuthConfig {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub file: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct BrokerTlsConfig {
    pub cert: String,
    pub key: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ApiConfig {
    /// Whether the HTTP management API is started. The API is
    /// kept around for external admin tools / scripts; the
    /// agent's own tools talk to the broker over an in-process
    /// channel and don't need this. Defaults to `true` for
    /// backward compatibility.
    #[serde(default = "default_api_enabled")]
    pub enabled: bool,
    pub host: String,
    pub port: u16,
    #[serde(default)]
    pub tls: bool,
    pub token: ApiTokenConfig,
}

fn default_api_enabled() -> bool {
    true
}

#[derive(Debug, Deserialize, Clone)]
pub struct ApiTokenConfig {
    pub secret: String,
    pub expire_secs: u64,
}

#[derive(Debug, Deserialize, Clone)]
pub struct AgentConfig {
    pub llm: LlmConfig,
    #[serde(default)]
    pub channels: ChannelsConfig,
    pub skills: SkillsConfig,
    /// OpenClaw 工作区根目录。`zeroclaw` 会在拼 system prompt
    /// 时自动读这里的 `AGENTS.md` / `SOUL.md` / `IDENTITY.md` /
    /// `TOOLS.md` / `USER.md` / `HEARTBEAT.md` / `MEMORY.md`。
    /// 相对路径以 broker 进程 cwd 为基准（与 `config/broker.yaml`
    /// 同套约定）；绝对路径直接使用。默认 `config/soul`。
    /// 首次启动时如果目录不存在，会自动创建并写入默认的
    /// SOUL 套件；用户随后可自由编辑。
    #[serde(default = "default_agent_workspace_dir")]
    pub workspace_dir: String,
    /// Identity 格式：`"openclaw"`（默认，多文件 markdown 注入）
    /// 或 `"aieos"`（单文件 JSON schema）。对应 zeroclaw 的
    /// `IdentityConfig::format`。
    #[serde(default = "default_identity_format")]
    pub identity_format: String,
    /// 语音输出改写配置：把 LLM 的 markdown / 富文本回复
    /// 转成 TTS 友好的版本。原始 `response` 始终保留（用于
    /// 日志、follow-up 分类、未来文本通道），只有送进
    /// `tts.speak()` 的字符串会走 transform。
    #[serde(default)]
    pub voice_output: VoiceOutputConfig,
}

fn default_agent_workspace_dir() -> String {
    "config/soul".to_string()
}

fn default_identity_format() -> String {
    "openclaw".to_string()
}

/// 语音输出改写配置：把 LLM 原始回复改写成 TTS 友好版本。
///
/// 原始 response 始终保留（用于日志、followup 分类、未来的
/// 文本通道）；只有送进 `tts.speak()` 的字符串会走 transform。
#[derive(Debug, Deserialize, Clone)]
pub struct VoiceOutputConfig {
    /// 主开关。`false` 时 transform 是 no-op，TTS 直接念原文。
    #[serde(default = "default_voice_output_enabled")]
    pub enabled: bool,
    /// 硬截断字符数（按 UTF-8 字符计）。超过会被 `truncate_suffix`
    /// 替代。Piper VITS 中文速度约 4-5 字/秒，120 字约 25-30 秒，
    /// 配合 `SOUL.md` "20 秒内" 留出一点余量。
    #[serde(default = "default_voice_output_max_chars")]
    pub max_chars: usize,
    /// 截断后追加的尾标，让用户听得出"被打断了"。
    #[serde(default = "default_voice_output_truncate_suffix")]
    pub truncate_suffix: String,
    /// 去掉 markdown 强调符号（`**` / `*` / `_` / `__`）。
    #[serde(default = "default_true")]
    pub strip_emphasis: bool,
    /// 去掉行首的 `#` / `##` / `###` 等 heading 前缀。
    #[serde(default = "default_true")]
    pub strip_headings: bool,
    /// 把 bullet list 转成 "第一，...。第二，...。" 的口述形式。
    #[serde(default = "default_true")]
    pub expand_lists: bool,
    /// 去掉 markdown 链接 `[text](url)` —— 只保留 `text`。
    #[serde(default = "default_true")]
    pub strip_markdown_links: bool,
    /// 去掉裸 URL（http/https/mqtt/ws 开头）。
    #[serde(default = "default_true")]
    pub drop_bare_urls: bool,
    /// 去掉行内 code（`` `xxx` ``）和 fenced code block
    /// （```` ```xxx``` ````）。完全删除（不留占位），
    /// 因为 TTS 念出反引号 / 英文代码体验极差。
    #[serde(default = "default_true")]
    pub drop_code: bool,
    /// 去掉 markdown 表格（`|` 列分隔）。
    #[serde(default = "default_true")]
    pub drop_tables: bool,
    /// 去掉常见 emoji 范围字符。
    #[serde(default = "default_true")]
    pub drop_emoji: bool,
    /// 把连续空白（多个空格 / 多个换行）合并成单空格。
    #[serde(default = "default_true")]
    pub collapse_whitespace: bool,
    /// 限定 TTS 文本为中文。
    ///
    /// 中文限定 TTS（如 sherpa-onnx matcha-icefall-zh-baker、VITS
    /// Piper 中文模型）只对中文字符发音清晰，遇到英文会按拼音硬
    /// 念 / 静默失败。开启本开关后：
    ///
    /// - `require_chinese_action: "warn"` (默认) —— CJK 比例低于
    ///   `require_chinese_min_ratio` 时记一条 warn 日志并把
    ///   非 CJK 字符从 TTS 文本里剥掉再发送。原始 response 不变。
    /// - `require_chinese_action: "drop"` —— 直接静默剥掉非 CJK
    ///   字符（不警告）。
    /// - `require_chinese_action: "pass"` —— 不做处理，让 TTS 自己
    ///   念。其它字段失效。
    #[serde(default)]
    pub require_chinese: bool,
    /// CJK 字符数 / 总可计数字符 比例的下限。低于此值触发剥字
    /// 逻辑。仅在 `require_chinese = true` 且 action ∈ {warn, drop}
    /// 时有效。范围 0.0-1.0，默认 0.5。
    #[serde(default = "default_require_chinese_min_ratio")]
    pub require_chinese_min_ratio: f32,
    /// `"warn"` (默认) | `"drop"` | `"pass"`。见 `require_chinese`。
    #[serde(default = "default_require_chinese_action")]
    pub require_chinese_action: String,
}

fn default_voice_output_enabled() -> bool {
    true
}
fn default_voice_output_max_chars() -> usize {
    120
}
fn default_voice_output_truncate_suffix() -> String {
    "，后面的内容省略了".to_string()
}
fn default_true() -> bool {
    true
}
fn default_require_chinese_min_ratio() -> f32 {
    0.5
}
fn default_require_chinese_action() -> String {
    "warn".to_string()
}

impl Default for VoiceOutputConfig {
    fn default() -> Self {
        Self {
            enabled: default_voice_output_enabled(),
            max_chars: default_voice_output_max_chars(),
            truncate_suffix: default_voice_output_truncate_suffix(),
            strip_emphasis: true,
            strip_headings: true,
            expand_lists: true,
            strip_markdown_links: true,
            drop_bare_urls: true,
            drop_code: true,
            drop_tables: true,
            drop_emoji: true,
            collapse_whitespace: true,
            require_chinese: false,
            require_chinese_min_ratio: default_require_chinese_min_ratio(),
            require_chinese_action: default_require_chinese_action(),
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
pub struct LlmConfig {
    pub api_url: String,
    pub model: String,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub max_tokens: Option<u32>,
    #[serde(default = "default_temperature")]
    pub temperature: f64,
}

fn default_temperature() -> f64 {
    0.7
}

#[derive(Debug, Deserialize, Clone, Default)]
pub struct ChannelsConfig {
    #[serde(default)]
    pub text: TextChannelConfig,
    #[serde(default)]
    pub qq: QqChannelConfig,
    #[serde(default)]
    pub voice: VoiceChannelConfig,
}

#[derive(Debug, Deserialize, Clone)]
pub struct TextChannelConfig {
    pub enabled: bool,
}

impl Default for TextChannelConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

#[derive(Debug, Deserialize, Clone, Default)]
pub struct QqChannelConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub onebot_url: Option<String>,
    #[serde(default)]
    pub authorized_users: Vec<String>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct VoiceChannelConfig {
    #[serde(default)]
    pub enabled: bool,
    /// "stub" (default) or "sherpa".
    #[serde(default = "default_engine")]
    pub engine: String,
    #[serde(default = "default_wake_word")]
    pub wake_word: String,
    #[serde(default = "default_sample_rate")]
    pub sample_rate: u32,
    #[serde(default = "default_capture_timeout_secs")]
    pub capture_timeout_secs: u64,
    #[serde(default = "default_followup_timeout_secs")]
    pub followup_timeout_secs: u64,
    /// TTS prompt played right after the wake word fires, before the
    /// mic is opened for user speech. The wake word itself doesn't
    /// tell the user "I heard you", so this acknowledgement ("在呢")
    /// closes the gap. Played only on the *initial* session turn; on
    /// follow-up turns the agent's question already serves the same
    /// role.
    #[serde(default = "default_wake_prompt")]
    pub wake_prompt: String,
    /// TTS prompt played when a wake word fires *while the agent is
    /// still processing the previous turn*. The agent keeps running
    /// — the wake is just acknowledged with this prompt ("正在执行
    /// 任务") and discarded, and normal flow resumes once the agent
    /// returns. Prevents the user from thinking their wake was lost
    /// and stacking fresh wake words on top of a busy agent.
    #[serde(default = "default_busy_prompt")]
    pub busy_prompt: String,
    #[serde(default = "default_timeout_prompt")]
    pub timeout_prompt: String,
    #[serde(default = "default_followup_patterns")]
    pub follow_up_patterns: Vec<String>,
    /// Aliases (e.g. "退出任务") that the KWS / voice loop should treat
    /// as a "stop current session" command instead of a normal wake.
    /// Matched case-insensitive substring against the detected alias.
    #[serde(default)]
    pub exit_wake_words: Vec<String>,

    // ── sherpa-onnx model paths ────────────────────────────────────────
    // KWS (KeywordSpotter) — independent model used for wake-word detection.
    // Recommended: sherpa-onnx-kws-zipformer-wenetspeech-3.3M-2024-01-01
    //   (WenetSpeech, Chinese, ~32 MB).
    #[serde(default)]
    pub kws_encoder: Option<String>,
    #[serde(default)]
    pub kws_decoder: Option<String>,
    #[serde(default)]
    pub kws_joiner: Option<String>,
    #[serde(default)]
    pub kws_tokens: Option<String>,
    /// Optional path to the model's bundled `keywords.txt` (pinyin-token
    /// lines). Combined with `kws_keywords_inline` if both are set.
    #[serde(default)]
    pub kws_keywords_file: Option<String>,
    /// Extra pinyin-token keywords to add on top of `kws_keywords_file`.
    /// Each entry: `<pinyin tokens separated by spaces> @<alias>`.
    #[serde(default)]
    pub kws_keywords_inline: Vec<String>,
    #[serde(default = "default_num_threads")]
    pub kws_num_threads: i32,
    #[serde(default = "default_provider")]
    pub kws_provider: String,
    #[serde(default)]
    pub kws_debug: bool,

    // ASR (OnlineRecognizer) — used after wake to transcribe user speech.
    // Recommended: sherpa-onnx-streaming-zipformer-bilingual-zh-en-2023-02-20
    #[serde(default)]
    pub asr_encoder: Option<String>,
    #[serde(default)]
    pub asr_decoder: Option<String>,
    #[serde(default)]
    pub asr_joiner: Option<String>,
    #[serde(default)]
    pub asr_tokens: Option<String>,
    #[serde(default = "default_num_threads")]
    pub num_threads: i32,
    #[serde(default = "default_provider")]
    pub provider: String,

    // TTS (OfflineTts) — used to speak the agent's responses.
    /// TTS backend: "vits" (default, Piper-style single-speaker),
    /// "kokoro" (multi-speaker, Chinese+English, native sherpa-onnx),
    /// or "matcha" (non-autoregressive, single-speaker, Chinese via
    /// matcha-icefall-zh-baker or English via matcha-icefall-en_US-ljspeech).
    #[serde(default = "default_tts_backend")]
    pub tts_backend: String,

    // VITS-only fields (used when `tts_backend = "vits"`, default).
    #[serde(default)]
    pub tts_model: Option<String>,
    #[serde(default)]
    pub tts_tokens: Option<String>,
    #[serde(default)]
    pub tts_data_dir: Option<String>,
    #[serde(default = "default_tts_noise_scale")]
    pub tts_noise_scale: f32,
    #[serde(default = "default_tts_noise_scale_w")]
    pub tts_noise_scale_w: f32,

    // Kokoro-only fields (used when `tts_backend = "kokoro"`).
    // Required: `tts_model`, `tts_tokens`, `tts_voices`, `tts_data_dir`,
    // `tts_dict_dir`. `tts_lexicons` is a list of lexicon files joined
    // into a comma-separated string at runtime (the sherpa-onnx C API
    // accepts a single string with comma-separated paths).
    #[serde(default)]
    pub tts_voices: Option<String>,
    #[serde(default)]
    pub tts_dict_dir: Option<String>,
    #[serde(default)]
    pub tts_lexicons: Vec<String>,

    // Matcha-only fields (used when `tts_backend = "matcha"`).
    // Matcha needs a separate vocoder ONNX (`tts_vocoder`) and a
    // comma-separated list of rule FSTs (`tts_rule_fsts`, e.g.
    // "phone.fst,date.fst,number.fst"). For zh-baker the lexicon
    // is a single `lexicon.txt`; we still take it from `tts_lexicons`
    // (list of size 1) to keep the YAML shape uniform.
    #[serde(default)]
    pub tts_vocoder: Option<String>,
    #[serde(default)]
    pub tts_rule_fsts: Vec<String>,

    // TTS fields shared by every backend.
    #[serde(default = "default_tts_length_scale")]
    pub tts_length_scale: f32,
    #[serde(default = "default_tts_speed")]
    pub tts_speed: f32,
    /// Speaker ID (Kokoro sid; ignored by single-speaker VITS / Matcha).
    /// Kokoro v1_1 supports 103 speakers (0-102); the Chinese female
    /// voices are sid 3-57, Chinese male 58-102. Default 3 = zf_001.
    #[serde(default = "default_tts_speaker_id")]
    pub tts_speaker_id: i32,
    #[serde(default)]
    pub tts_debug: bool,

    // ── cpal device selection + VAD tuning ────────────────────────────
    #[serde(default)]
    pub audio_input_device: Option<String>,
    #[serde(default)]
    pub audio_output_device: Option<String>,
    // Silero VAD (sherpa-onnx VoiceActivityDetector). When `vad_model` is
    // `Some`, it is used for endpoint detection during user capture; far
    // more accurate than energy thresholding in noisy environments.
    // Recommended model: `silero_vad_v5.onnx` from the sherpa-onnx
    // asr-models release (~2.3 MB). When `vad_model` is `None`, the
    // fallback RMS energy VAD below is used.
    #[serde(default)]
    pub vad_model: Option<String>,
    #[serde(default = "default_vad_threshold")]
    pub vad_threshold: f32,
    #[serde(default = "default_vad_min_silence_ms")]
    pub vad_min_silence_ms: u32,
    #[serde(default = "default_vad_min_speech_ms")]
    pub vad_min_speech_ms: u32,
    #[serde(default = "default_vad_max_speech_secs")]
    pub vad_max_speech_secs: f32,
    #[serde(default = "default_vad_num_threads")]
    pub vad_num_threads: i32,
    #[serde(default = "default_vad_buffer_secs")]
    pub vad_buffer_secs: f32,
    // Fallback RMS energy VAD (used when `vad_model` is `None`).
    #[serde(default = "default_rms_threshold")]
    pub rms_threshold: f32,
    #[serde(default = "default_silence_ms")]
    pub silence_ms: u32,
    #[serde(default = "default_pre_speech_ms")]
    pub pre_speech_ms: u32,

    // ── stub-only knobs (ignored when engine = "sherpa") ───────────────
    /// Optional canned utterances used by the stub ASR for local rehearsal.
    #[serde(default)]
    pub stub_utterances: Vec<String>,
    /// Wake auto-fire interval used by the stub wake detector (seconds).
    #[serde(default = "default_stub_wake_interval_secs")]
    pub stub_wake_interval_secs: u64,
}

impl Default for VoiceChannelConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            engine: default_engine(),
            wake_word: default_wake_word(),
            sample_rate: default_sample_rate(),
            capture_timeout_secs: default_capture_timeout_secs(),
            followup_timeout_secs: default_followup_timeout_secs(),
            wake_prompt: default_wake_prompt(),
            busy_prompt: default_busy_prompt(),
            timeout_prompt: default_timeout_prompt(),
            follow_up_patterns: default_followup_patterns(),
            exit_wake_words: Vec::new(),
            kws_encoder: None,
            kws_decoder: None,
            kws_joiner: None,
            kws_tokens: None,
            kws_keywords_file: None,
            kws_keywords_inline: Vec::new(),
            kws_num_threads: default_num_threads(),
            kws_provider: default_provider(),
            kws_debug: false,
            asr_encoder: None,
            asr_decoder: None,
            asr_joiner: None,
            asr_tokens: None,
            num_threads: default_num_threads(),
            provider: default_provider(),
            tts_model: None,
            tts_tokens: None,
            tts_data_dir: None,
            tts_length_scale: default_tts_length_scale(),
            tts_speed: default_tts_speed(),
            tts_noise_scale: default_tts_noise_scale(),
            tts_noise_scale_w: default_tts_noise_scale_w(),
            tts_debug: false,
            tts_backend: default_tts_backend(),
            tts_voices: None,
            tts_dict_dir: None,
            tts_lexicons: Vec::new(),
            tts_speaker_id: default_tts_speaker_id(),
            tts_vocoder: None,
            tts_rule_fsts: Vec::new(),
            audio_input_device: None,
            audio_output_device: None,
            vad_model: None,
            vad_threshold: default_vad_threshold(),
            vad_min_silence_ms: default_vad_min_silence_ms(),
            vad_min_speech_ms: default_vad_min_speech_ms(),
            vad_max_speech_secs: default_vad_max_speech_secs(),
            vad_num_threads: default_vad_num_threads(),
            vad_buffer_secs: default_vad_buffer_secs(),
            rms_threshold: default_rms_threshold(),
            silence_ms: default_silence_ms(),
            pre_speech_ms: default_pre_speech_ms(),
            stub_utterances: Vec::new(),
            stub_wake_interval_secs: default_stub_wake_interval_secs(),
        }
    }
}

fn default_engine() -> String {
    "stub".into()
}

fn default_wake_word() -> String {
    "你好小助".into()
}

fn default_sample_rate() -> u32 {
    16000
}

fn default_capture_timeout_secs() -> u64 {
    8
}

fn default_followup_timeout_secs() -> u64 {
    6
}

fn default_timeout_prompt() -> String {
    "等待超时".into()
}

fn default_wake_prompt() -> String {
    "在呢".into()
}

fn default_busy_prompt() -> String {
    "正在执行任务".into()
}

fn default_followup_patterns() -> Vec<String> {
    vec![
        r"[?？]\s*$".into(),
        r"请确认".into(),
        r"是否".into(),
        r"对吗".into(),
        r"请告诉我".into(),
        r"请提供".into(),
    ]
}

fn default_stub_wake_interval_secs() -> u64 {
    5
}

fn default_tts_length_scale() -> f32 {
    1.0
}

fn default_tts_speed() -> f32 {
    1.0
}

fn default_tts_noise_scale() -> f32 {
    0.667
}

fn default_tts_noise_scale_w() -> f32 {
    0.8
}

fn default_tts_backend() -> String {
    "vits".into()
}

fn default_tts_speaker_id() -> i32 {
    3
}

fn default_num_threads() -> i32 {
    2
}

fn default_provider() -> String {
    "cpu".into()
}

fn default_rms_threshold() -> f32 {
    0.01
}

fn default_silence_ms() -> u32 {
    500
}

fn default_pre_speech_ms() -> u32 {
    300
}

fn default_vad_threshold() -> f32 {
    0.5
}

fn default_vad_min_silence_ms() -> u32 {
    500
}

fn default_vad_min_speech_ms() -> u32 {
    250
}

fn default_vad_max_speech_secs() -> f32 {
    20.0
}

fn default_vad_num_threads() -> i32 {
    1
}

fn default_vad_buffer_secs() -> f32 {
    30.0
}

#[derive(Debug, Deserialize, Clone)]
pub struct SkillsConfig {
    pub mqtt_manager: MqttManagerSkillConfig,
}

#[derive(Debug, Deserialize, Clone)]
pub struct MqttManagerSkillConfig {
    pub enabled: bool,
    pub api_base_url: String,
    pub api_token: String,
}

impl Config {
    pub fn load<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read config file {}", path.display()))?;
        let cfg: Config = serde_yaml::from_str(&content)
            .with_context(|| format!("failed to parse config file {}", path.display()))?;
        Ok(cfg)
    }

    pub fn load_default() -> Result<Self> {
        let path = std::env::var(CONFIG_PATH_ENV).unwrap_or_else(|_| DEFAULT_CONFIG_PATH.into());
        Self::load(path)
    }

    pub fn mqtt_bind_addr(&self) -> String {
        format!("{}:{}", self.broker.host, self.broker.mqtt_port)
    }

    pub fn api_bind_addr(&self) -> String {
        format!("{}:{}", self.api.host, self.api.port)
    }
}
