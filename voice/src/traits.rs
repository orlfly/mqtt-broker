use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

/// The wake event that just fired. The `keyword` is the human-readable
/// alias (e.g. "你好小金", "退出任务") declared in the KWS keywords
/// file. `kind` lets the orchestrator treat exit-style wake words
/// differently from session-starting ones.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WakeEvent {
    pub kind: WakeKind,
    /// Human-readable alias of the keyword (after the `@` in the KWS
    /// keywords file / inline entry). When the model matches a bundled
    /// keyword without an alias, the engine falls back to the raw
    /// `keyword` string from sherpa-onnx.
    pub keyword: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WakeKind {
    /// A normal session-starting wake word (e.g. "你好小金").
    Wake,
    /// An exit-style wake word that should terminate the current
    /// session/turn (e.g. "退出任务"). The orchestrator short-circuits
    /// the capture/ASR/agent pipeline and returns to listening.
    Exit,
}

impl WakeEvent {
    pub fn wake(keyword: impl Into<String>) -> Self {
        Self {
            kind: WakeKind::Wake,
            keyword: keyword.into(),
        }
    }
    pub fn exit(keyword: impl Into<String>) -> Self {
        Self {
            kind: WakeKind::Exit,
            keyword: keyword.into(),
        }
    }
    /// Match a detected keyword against a list of configured "exit"
    /// aliases. Case-insensitive substring match — Chinese aliases are
    /// usually exact, but we keep the option open for Latin phrases.
    pub fn classify(keyword: &str, exit_aliases: &[String]) -> Self {
        let lower = keyword.to_lowercase();
        let is_exit = exit_aliases
            .iter()
            .any(|alias| !alias.is_empty() && lower.contains(&alias.to_lowercase()));
        if is_exit {
            Self::exit(keyword)
        } else {
            Self::wake(keyword)
        }
    }
}

/// Wake-word detector. Blocks until the configured wake phrase is heard,
/// then returns which keyword fired.
///
/// `shutdown` is a process-wide cancellation flag. Implementations
/// MUST poll it periodically (the orchestrator relies on
/// responsiveness here for Ctrl+C handling — see
/// `agent::voice_loop::VoiceLoop::run`) and return
/// `Err(anyhow::anyhow!("wake detector shutdown requested"))` when
/// it flips to `true`. For blocking implementations (e.g. the
/// sherpa-onnx KWS, which runs on a `spawn_blocking` thread), the
/// flag MUST be checked inside the inner consumer loop — futures
/// cancelled by `select!` do not abort `spawn_blocking` tasks, so
/// without the explicit check the audio thread keeps the device
/// open and the process hangs on exit.
#[async_trait]
pub trait WakeDetector: Send + Sync {
    async fn wait_for_wake(
        &self,
        shutdown: Arc<AtomicBool>,
    ) -> anyhow::Result<WakeEvent>;
}

/// Microphone capture with VAD-based endpointing.
///
/// Implementations should open the capture stream, run VAD until silence
/// (utterance ended) or until `timeout` elapses, and return the captured
/// PCM samples. Returning `Ok(Vec::new())` is treated as a timeout by the
/// orchestrator.
///
/// `shutdown` is the same process-wide cancellation flag passed to
/// `WakeDetector::wait_for_wake`. Implementations that run a blocking
/// loop (e.g. the cpal capture consumer thread) MUST poll it
/// periodically and return `Ok(Vec::new())` when it flips — otherwise
/// Ctrl+C during recording will block for up to `timeout` before the
/// process exits.
#[async_trait]
pub trait AudioCapture: Send + Sync {
    async fn capture_until_silence(
        &self,
        timeout: Duration,
        shutdown: Arc<AtomicBool>,
    ) -> anyhow::Result<Vec<f32>>;
}

/// ASR transcriber. Converts PCM samples into text.
#[async_trait]
pub trait AsrTranscriber: Send + Sync {
    async fn transcribe(&self, samples: &[f32]) -> anyhow::Result<String>;
}

/// TTS player. Synthesizes the text and plays it out, returning **after**
/// playback finishes (so the caller can safely open the microphone again
/// without echo feedback).
///
/// `shutdown` is the same process-wide cancellation flag. The blocking
/// cpal playback path polls it inside its drain loop so Ctrl+C during a
/// long TTS clip (currently the worst-case hang path — TTS playback
/// can run for 10+ s on a 5 s clip when device rate is 8× the model
/// rate) bails out within ~10 ms.
#[async_trait]
pub trait TtsSpeaker: Send + Sync {
    async fn speak(
        &self,
        text: &str,
        shutdown: Arc<AtomicBool>,
    ) -> anyhow::Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_wake_when_no_exit_words_configured() {
        let e = WakeEvent::classify("你好小金", &[]);
        assert_eq!(e.kind, WakeKind::Wake);
        assert_eq!(e.keyword, "你好小金");
    }

    #[test]
    fn classify_wake_when_alias_does_not_match() {
        let e = WakeEvent::classify("你好小金", &["退出任务".into()]);
        assert_eq!(e.kind, WakeKind::Wake);
    }

    #[test]
    fn classify_exit_when_alias_substring_matches() {
        let e = WakeEvent::classify("退出任务", &["退出任务".into()]);
        assert_eq!(e.kind, WakeKind::Exit);
    }

    #[test]
    fn classify_exit_is_case_insensitive() {
        let e = WakeEvent::classify("Quit Now", &["quit".into()]);
        assert_eq!(e.kind, WakeKind::Exit);
    }

    #[test]
    fn classify_skips_empty_exit_aliases() {
        let e = WakeEvent::classify("你好小金", &["".into()]);
        assert_eq!(e.kind, WakeKind::Wake);
    }
}
