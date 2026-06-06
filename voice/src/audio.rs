use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use tokio::time::sleep;

use crate::traits::AudioCapture;

/// Original sync helper retained for back-compat (file-based audio, VAD util).
pub struct AudioIO;

impl AudioIO {
    pub fn new() -> Self {
        Self
    }

    pub fn play(audio: &[f32]) -> anyhow::Result<()> {
        tracing::info!("Playing audio ({} samples)", audio.len());
        Ok(())
    }

    pub fn vad(audio: &[f32]) -> bool {
        let energy: f32 = audio.iter().map(|s| s * s).sum();
        let threshold = 0.01 * audio.len() as f32;
        energy > threshold
    }
}

impl Default for AudioIO {
    fn default() -> Self {
        Self::new()
    }
}

/// Stub microphone capture.
///
/// Strategy:
/// 1. Sleep `simulated_speech_duration` (default 800ms).
/// 2. Return `simulated_samples` of dummy PCM.
///
/// If `simulated_speech_duration` exceeds the orchestrator's timeout,
/// the orchestrator's `tokio::time::timeout(...)` cancels this future and
/// reports a timeout.
///
/// Use `with_scripted_durations` to cycle through different behaviours,
/// e.g. `[800ms, 9_000ms, 800ms]` will succeed, then timeout, then succeed.
pub struct StubAudioCapture {
    sample_rate: u32,
    durations: Mutex<Vec<Duration>>,
    cycle: bool,
    cursor: Mutex<usize>,
}

impl StubAudioCapture {
    pub fn new(sample_rate: u32, simulated_speech_duration: Duration) -> Self {
        Self {
            sample_rate,
            durations: Mutex::new(vec![simulated_speech_duration]),
            cycle: true,
            cursor: Mutex::new(0),
        }
    }

    pub fn with_scripted_durations(sample_rate: u32, durations: Vec<Duration>) -> Self {
        Self {
            sample_rate,
            durations: Mutex::new(durations),
            cycle: true,
            cursor: Mutex::new(0),
        }
    }

    fn next_duration(&self) -> Duration {
        let durations = self.durations.lock().unwrap();
        let mut cursor = self.cursor.lock().unwrap();
        let len = durations.len().max(1);
        let idx = *cursor % len;
        if self.cycle {
            *cursor = (*cursor + 1) % len;
        } else if *cursor < len {
            *cursor += 1;
        }
        durations.get(idx).copied().unwrap_or(Duration::from_millis(800))
    }
}

#[async_trait]
impl AudioCapture for StubAudioCapture {
    async fn capture_until_silence(
        &self,
        timeout: Duration,
        shutdown: Arc<AtomicBool>,
    ) -> anyhow::Result<Vec<f32>> {
        let speech_dur = self.next_duration();
        tracing::info!(
            "[capture stub] opening microphone (sr={}, timeout={:?}, scripted_speech={:?})",
            self.sample_rate,
            timeout,
            speech_dur
        );
        // Honor the shutdown flag in 50 ms slices so the stub
        // doesn't outlast Ctrl+C. Mirrors the stub wake detector's
        // behavior.
        let mut slept = Duration::ZERO;
        let slice = Duration::from_millis(50);
        while slept < speech_dur {
            if shutdown.load(Ordering::SeqCst) {
                tracing::info!("[capture stub] shutdown requested, returning empty buffer");
                return Ok(Vec::new());
            }
            let remain = speech_dur - slept;
            sleep(remain.min(slice)).await;
            slept += remain.min(slice);
        }
        let samples = (speech_dur.as_millis() as usize * self.sample_rate as usize) / 1000;
        tracing::info!("[capture stub] captured {} samples", samples);
        Ok(vec![0.0; samples])
    }
}
