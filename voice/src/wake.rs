use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use tokio::time::sleep;

use crate::traits::{WakeDetector, WakeEvent};

/// Stub wake detector that fires after a fixed interval. Useful for
/// validating the state machine end-to-end without a real KWS engine.
///
/// Replace with e.g. Picovoice Porcupine, openWakeWord, or a streaming
/// sherpa-onnx hotword model for production.
pub struct StubWakeDetector {
    interval: Duration,
    wake_word: String,
    exit_words: Vec<String>,
}

impl StubWakeDetector {
    pub fn new(wake_word: impl Into<String>, interval: Duration) -> Self {
        Self {
            wake_word: wake_word.into(),
            interval,
            exit_words: Vec::new(),
        }
    }

    /// Register aliases that should be reported as `WakeKind::Exit` when
    /// they "fire" (i.e. when the stub decides to alternate).
    pub fn with_exit_words(mut self, words: Vec<String>) -> Self {
        self.exit_words = words;
        self
    }
}

#[async_trait]
impl WakeDetector for StubWakeDetector {
    async fn wait_for_wake(
        &self,
        shutdown: Arc<AtomicBool>,
    ) -> anyhow::Result<WakeEvent> {
        // Round-robin: alternate the configured main wake word with any
        // registered exit words so integration tests can exercise both
        // code paths without a real KWS.
        let exit = if self.exit_words.is_empty() {
            None
        } else {
            // Use a coarse time-based selection: every other firing flips.
            let now_nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let idx = (now_nanos / self.interval.as_nanos().max(1)) as usize;
            self.exit_words.get(idx % self.exit_words.len()).cloned()
        };
        let chosen = exit.clone().unwrap_or_else(|| self.wake_word.clone());
        tracing::info!(
            "[wake stub] listening for wake word '{}' (will auto-fire in {:?})",
            self.wake_word,
            self.interval
        );
        // Honor the shutdown flag so the stub doesn't outlast Ctrl+C.
        // The sleep is broken into 100 ms slices so shutdown latency
        // is bounded to ~100 ms in the worst case.
        let mut slept = Duration::ZERO;
        let slice = Duration::from_millis(100);
        while slept < self.interval {
            if shutdown.load(Ordering::SeqCst) {
                anyhow::bail!("wake stub shutdown requested");
            }
            let remain = self.interval - slept;
            sleep(remain.min(slice)).await;
            slept += remain.min(slice);
        }
        let event = WakeEvent::classify(&chosen, &self.exit_words);
        tracing::info!(
            "[wake stub] wake fired: alias='{}' kind={:?}",
            event.keyword, event.kind
        );
        Ok(event)
    }
}

/// Scripted wake detector that fires immediately N times and then waits
/// forever (useful for integration tests).
pub struct ScriptedWakeDetector {
    remaining: Mutex<usize>,
}

impl ScriptedWakeDetector {
    pub fn new(times: usize) -> Self {
        Self {
            remaining: Mutex::new(times),
        }
    }
}

#[async_trait]
impl WakeDetector for ScriptedWakeDetector {
    async fn wait_for_wake(
        &self,
        shutdown: Arc<AtomicBool>,
    ) -> Result<WakeEvent> {
        let should_fire = {
            let mut g = self.remaining.lock().unwrap();
            if *g == 0 {
                false
            } else {
                *g -= 1;
                true
            }
        };
        if should_fire {
            Ok(WakeEvent::wake("scripted"))
        } else {
            // Block until shutdown. `pending` is a future that never
            // resolves on its own, so we periodically `yield_now` to
            // give the runtime a chance to check `shutdown` between
            // iterations.
            loop {
                if shutdown.load(Ordering::SeqCst) {
                    anyhow::bail!("scripted wake shutdown requested");
                }
                tokio::task::yield_now().await;
                // Tiny sleep to avoid a 100 % CPU spin in tests.
                sleep(Duration::from_millis(10)).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    /// If shutdown is pre-set, `StubWakeDetector::wait_for_wake`
    /// must return an error within at most a few hundred ms —
    /// never the full `interval`. This is the in-process analogue
    /// of the KWS's "consumer loop sees the flag and bails" path.
    #[tokio::test]
    async fn stub_bails_when_shutdown_pre_set() {
        let det = StubWakeDetector::new("hi".to_string(), Duration::from_secs(10));
        let shutdown = Arc::new(AtomicBool::new(true));
        let start = Instant::now();
        let result = det.wait_for_wake(shutdown).await;
        let elapsed = start.elapsed();
        assert!(result.is_err(), "expected bail, got {result:?}");
        assert!(
            elapsed < Duration::from_secs(1),
            "stub waited too long ({elapsed:?}) before honoring shutdown"
        );
    }

    /// If shutdown flips mid-wait, the stub should still bail
    /// promptly (well before the configured `interval`).
    #[tokio::test]
    async fn stub_bails_when_shutdown_flips_mid_wait() {
        let det = StubWakeDetector::new("hi".to_string(), Duration::from_secs(10));
        let shutdown = Arc::new(AtomicBool::new(false));
        let s2 = shutdown.clone();
        tokio::spawn(async move {
            sleep(Duration::from_millis(50)).await;
            s2.store(true, Ordering::SeqCst);
        });
        let start = Instant::now();
        let result = det.wait_for_wake(shutdown).await;
        let elapsed = start.elapsed();
        assert!(result.is_err(), "expected bail, got {result:?}");
        assert!(
            elapsed < Duration::from_secs(1),
            "stub took {elapsed:?} to honor shutdown"
        );
    }

    /// `ScriptedWakeDetector` after exhausting all scripted
    /// firings must block on the shutdown flag, not on
    /// `pending()`.
    #[tokio::test]
    async fn scripted_bails_when_shutdown_flips_after_exhausted() {
        let det = ScriptedWakeDetector::new(0); // zero firings
        let shutdown = Arc::new(AtomicBool::new(false));
        let s2 = shutdown.clone();
        tokio::spawn(async move {
            sleep(Duration::from_millis(50)).await;
            s2.store(true, Ordering::SeqCst);
        });
        let start = Instant::now();
        let result = det.wait_for_wake(shutdown).await;
        let elapsed = start.elapsed();
        assert!(result.is_err(), "expected bail, got {result:?}");
        assert!(
            elapsed < Duration::from_secs(1),
            "scripted took {elapsed:?} to honor shutdown"
        );
    }
}
