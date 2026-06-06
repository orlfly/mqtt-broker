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
    pub host: String,
    pub port: u16,
    #[serde(default)]
    pub tls: bool,
    pub token: ApiTokenConfig,
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

    // TTS (OfflineTts VITS) — used to speak the agent's responses.
    #[serde(default)]
    pub tts_model: Option<String>,
    #[serde(default)]
    pub tts_tokens: Option<String>,
    #[serde(default)]
    pub tts_data_dir: Option<String>,
    #[serde(default = "default_tts_length_scale")]
    pub tts_length_scale: f32,
    #[serde(default = "default_tts_speed")]
    pub tts_speed: f32,
    #[serde(default = "default_tts_noise_scale")]
    pub tts_noise_scale: f32,
    #[serde(default = "default_tts_noise_scale_w")]
    pub tts_noise_scale_w: f32,
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
