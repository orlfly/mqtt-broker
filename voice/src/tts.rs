use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use tokio::time::{sleep, Duration};

use crate::traits::TtsSpeaker;

pub struct TtsEngine {
    #[allow(dead_code)]
    model_path: String,
    #[allow(dead_code)]
    speaker: u32,
    #[allow(dead_code)]
    speed: f32,
}

impl TtsEngine {
    pub fn new(model_path: &str, speaker: u32, speed: f32) -> Self {
        Self {
            model_path: model_path.to_string(),
            speaker,
            speed,
        }
    }

    pub fn init(model_path: &str) -> anyhow::Result<Self> {
        tracing::info!("Initializing TTS engine with model: {}", model_path);
        Ok(Self {
            model_path: model_path.to_string(),
            speaker: 0,
            speed: 1.0,
        })
    }

    pub fn config(&self) -> TtsConfig {
        TtsConfig {
            model_path: self.model_path.clone(),
            speaker: self.speaker,
            speed: self.speed,
        }
    }
}

#[derive(Debug, Clone)]
pub struct TtsConfig {
    pub model_path: String,
    pub speaker: u32,
    pub speed: f32,
}

pub type SharedTtsEngine = Arc<TtsEngine>;

/// Stub TTS that "plays" by logging and sleeping proportional to text
/// length, so the orchestrator behaves realistically (mic stays closed
/// during simulated playback, no echo into ASR).
///
/// Replace with sherpa-onnx VITS + cpal output stream for production.
pub struct StubTts {
    char_duration: Duration,
}

impl StubTts {
    pub fn new(char_duration: Duration) -> Self {
        Self { char_duration }
    }
}

impl Default for StubTts {
    fn default() -> Self {
        Self::new(Duration::from_millis(60))
    }
}

#[async_trait]
impl TtsSpeaker for StubTts {
    async fn speak(
        &self,
        text: &str,
        shutdown: Arc<AtomicBool>,
    ) -> anyhow::Result<()> {
        let chars = text.chars().count();
        let play_for = self.char_duration.saturating_mul(chars as u32);
        tracing::info!("[tts stub] >>> {}", text);
        tracing::debug!("[tts stub] simulated playback {:?}", play_for);
        // Honor the shutdown flag in 50 ms slices so the stub
        // doesn't outlast Ctrl+C during a long simulated
        // playback.
        let mut slept = Duration::ZERO;
        let slice = Duration::from_millis(50);
        while slept < play_for {
            if shutdown.load(Ordering::SeqCst) {
                tracing::info!("[tts stub] shutdown requested, aborting playback");
                return Ok(());
            }
            let remain = play_for - slept;
            sleep(remain.min(slice)).await;
            slept += remain.min(slice);
        }
        tracing::debug!("[tts stub] playback finished");
        Ok(())
    }
}
