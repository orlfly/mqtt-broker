use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use regex::Regex;
use tokio::time::timeout;
use tracing::{info, warn};
use voice::{AsrTranscriber, AudioCapture, TtsSpeaker, WakeDetector, WakeEvent, WakeKind};
use zeroclaw::agent::Agent;

/// Decides whether an agent response is asking the user for additional
/// input (so the loop should re-open the mic without waiting for the
/// wake word).
pub struct FollowupClassifier {
    patterns: Vec<Regex>,
}

impl FollowupClassifier {
    pub fn new(raw_patterns: &[String]) -> Self {
        let patterns = raw_patterns
            .iter()
            .filter_map(|p| match Regex::new(p) {
                Ok(r) => Some(r),
                Err(e) => {
                    warn!("follow-up pattern {:?} failed to compile: {}", p, e);
                    None
                }
            })
            .collect();
        Self { patterns }
    }

    pub fn expects_followup(&self, response: &str) -> bool {
        let trimmed = response.trim();
        if trimmed.is_empty() {
            return false;
        }
        self.patterns.iter().any(|re| re.is_match(trimmed))
    }
}

pub struct VoiceLoopConfig {
    pub capture_timeout: Duration,
    pub followup_timeout: Duration,
    /// TTS prompt played right after the wake word fires, before the
    /// mic is opened for user speech (e.g. "在呢"). Played only on
    /// the *initial* session turn — follow-up turns skip it because
    /// the agent's question already serves the same role.
    pub wake_prompt: String,
    /// TTS prompt played when a wake word fires *while the agent is
    /// still processing the previous turn* (e.g. "正在执行任务"). The
    /// agent keeps running; the wake is just acknowledged with this
    /// prompt and discarded, and normal flow resumes once the agent
    /// returns. Ignored when the agent finished before the wake
    /// could be observed (in which case we just return to listening
    /// for the next wake).
    pub busy_prompt: String,
    pub timeout_prompt: String,
    /// Aliases that the wake detector should classify as `WakeKind::Exit`
    /// (e.g. "退出任务"). When one of these fires, the current session
    /// (if any) is short-circuited and we return to waiting.
    pub exit_wake_words: Vec<String>,
    /// Optional TTS confirmation played when an exit-style wake word fires.
    pub exit_prompt: Option<String>,
}

pub struct VoiceLoop {
    agent: Agent,
    wake: Arc<dyn WakeDetector>,
    capture: Arc<dyn AudioCapture>,
    asr: Arc<dyn AsrTranscriber>,
    tts: Arc<dyn TtsSpeaker>,
    classifier: FollowupClassifier,
    cfg: VoiceLoopConfig,
    /// Process-wide cancellation flag. Flipped to `true` by the
    /// Ctrl+C handler in `agent::main`; the wake detector's inner
    /// loop polls it and the orchestrator's outer loop also checks
    /// it between sessions so the next `wake.wait_for_wake` returns
    /// promptly.
    shutdown: Arc<AtomicBool>,
}

impl VoiceLoop {
    #[allow(clippy::too_many_arguments)] // shutdown is the 8th — every argument is genuinely independent and used in the orchestrator
    pub fn new(
        agent: Agent,
        wake: Arc<dyn WakeDetector>,
        capture: Arc<dyn AudioCapture>,
        asr: Arc<dyn AsrTranscriber>,
        tts: Arc<dyn TtsSpeaker>,
        classifier: FollowupClassifier,
        cfg: VoiceLoopConfig,
        shutdown: Arc<AtomicBool>,
    ) -> Self {
        Self {
            agent,
            wake,
            capture,
            asr,
            tts,
            classifier,
            cfg,
            shutdown,
        }
    }

    /// Outer loop: wait for wake word, then run a "session" of one or
    /// more turns (until the agent's response does not ask the user to
    /// follow up).
    pub async fn run(&mut self) -> Result<()> {
        loop {
            if self.shutdown.load(Ordering::SeqCst) {
                info!("[voice loop] shutdown requested, exiting");
                return Ok(());
            }
            info!("[voice loop] state = WaitingForWake");
            let raw = match self.wake.wait_for_wake(self.shutdown.clone()).await {
                Ok(e) => e,
                Err(e) => {
                    // The wake detector (especially the sherpa-onnx
                    // KWS) bails with an error when the shutdown
                    // flag flips. Translate that into a clean exit
                    // so the process can terminate normally.
                    if self.shutdown.load(Ordering::SeqCst) {
                        info!("[voice loop] wake detector exited via shutdown, returning Ok");
                        return Ok(());
                    }
                    // Any other error from the wake detector is
                    // treated as a transient failure — most
                    // commonly the KWS failing to open the input
                    // device (e.g. a USB mic that briefly
                    // disappeared from cpal's view mid-session,
                    // which is a known PipeWire quirk — see the
                    // ALSA-sees-the-device-but-cpal-doesn't hint
                    // in `find_input_device`). The pre-fix code
                    // returned the error here, which propagated
                    // all the way out of `voice.run().await` and
                    // killed the process; the user-facing symptom
                    // was "after one bad capture, the whole
                    // voice loop dies". Log + sleep + retry
                    // instead so the loop survives a flaky mic
                    // and recovers automatically when the device
                    // comes back.
                    warn!(
                        "[voice loop] wake detector errored: {} — retrying in 1s",
                        e
                    );
                    // Diagnostic: snapshot what cpal + pactl see
                    // at the moment of failure. Comparing this
                    // row to the matching row printed 1 s later
                    // (after the sleep + next retry) tells us
                    // whether the device came back on its own
                    // (PipeWire self-heal) or whether it's still
                    // gone (real unplug / udev issue).
                    voice::audio_devices::log_runtime_device_state("after_wake_error");
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    voice::audio_devices::log_runtime_device_state("before_wake_retry");
                    continue;
                }
            };
            // The KWS layer reports the alias (e.g. "你好小金", "退出任务")
            // but the agent's policy decides which aliases count as exit
            // words. Re-classify here so the policy lives in one place.
            let event = if matches!(raw.kind, WakeKind::Wake) && !self.cfg.exit_wake_words.is_empty()
            {
                WakeEvent::classify(&raw.keyword, &self.cfg.exit_wake_words)
            } else {
                raw
            };
            info!(
                "[voice loop] wake fired: alias={:?} kind={:?}",
                event.keyword, event.kind
            );
            match event.kind {
                WakeKind::Exit => {
                    if let Some(prompt) = &self.cfg.exit_prompt {
                        if let Err(e) = self
                            .tts
                            .speak(prompt, self.shutdown.clone())
                            .await
                        {
                            warn!("[voice loop] exit prompt TTS failed: {}", e);
                        }
                    }
                    info!("[voice loop] exit wake word → session skipped, returning to listen");
                }
                WakeKind::Wake => {
                    // Acknowledge the wake BEFORE opening the mic so
                    // the user has confirmation that the system
                    // heard them, and so the wake-word audio doesn't
                    // bleed into the capture buffer. While this TTS
                    // is playing the KWS / capture streams are both
                    // closed (we're between wait_for_wake and the
                    // first capture), so no wake/ASR processing can
                    // happen — that's the "TTS 播报期间不处理唤醒和
                    // 用户语音信息的 asr" requirement.
                    if !self.cfg.wake_prompt.is_empty() {
                        if let Err(e) = self
                            .tts
                            .speak(&self.cfg.wake_prompt, self.shutdown.clone())
                            .await
                        {
                            warn!("[voice loop] wake prompt TTS failed: {}", e);
                        }
                    }
                    // Diagnostic: snapshot just before we open the
                    // mic for the user's reply. If the KWS path
                    // itself was fine (USB MIC was visible during
                    // wait_for_wake) but the *next* open fails
                    // (capture_until_silence), the diff between
                    // this row and the after_capture_error row
                    // pinpoints exactly when PipeWire dropped the
                    // device.
                    voice::audio_devices::log_runtime_device_state("after_wake_prompt");
                    if let Err(e) = self.run_session().await {
                        warn!("[voice loop] session aborted: {}", e);
                    }
                }
            }
        }
    }

    async fn run_session(&mut self) -> Result<()> {
        let mut expect_followup = false;
        let mut first_iter = true;
        loop {
            let listen_timeout = if expect_followup {
                self.cfg.followup_timeout
            } else {
                self.cfg.capture_timeout
            };
            info!(
                "[voice loop] state = Capturing ({}, timeout={:?})",
                if expect_followup { "follow-up" } else { "initial" },
                listen_timeout
            );

            // Diagnostic: snapshot just before each capture open.
            // The user-reported symptom is "after TTS finishes,
            // the next capture can't find the mic" — this row
            // captures the state at that moment so we can diff
            // it against a later "after capture" row (printed
            // from `capture_and_recognize` on error) to see
            // whether cpal is the one lying (says device gone
            // when it isn't) or whether the device really did
            // disappear from the host.
            if first_iter {
                voice::audio_devices::log_runtime_device_state("before_capture_initial");
                first_iter = false;
            } else {
                voice::audio_devices::log_runtime_device_state("before_capture_followup");
            }

            let heard = self.capture_and_recognize(listen_timeout).await?;
            let Some(user_text) = heard else {
                // Timeout: tell the user, then exit session → wait for wake.
                self.tts
                    .speak(&self.cfg.timeout_prompt, self.shutdown.clone())
                    .await?;
                return Ok(());
            };

            info!("[voice loop] state = AgentTurn, user_text={:?}", user_text);
            // Run the agent but race a wake detector against it, so
            // the user can keep saying the wake word while the LLM
            // is thinking and hear a "busy" acknowledgement instead
            // of silence. The agent future keeps running in the
            // background; the wake future is re-armed on every hit
            // so the user can stack multiple busy acknowledgements
            // if they keep talking. The agent result is captured
            // exactly once when it finally resolves.
            let response = self.run_agent_with_busy_intercept(&user_text).await?;
            info!("[voice loop] LLM responded: {:?}", response);

            info!("[voice loop] state = Speaking, response={:?}", response);
            // While this response TTS is playing, neither the KWS
            // nor the capture stream is open — see the comment in
            // `run` about the "TTS 播报期间不处理唤醒和用户语音信息
            // 的 asr" rule. The next thing we do is either go back
            // to wait_for_wake (session done) or recurse into
            // capture_and_recognize for a follow-up turn.
            self.tts.speak(&response, self.shutdown.clone()).await?;
            // Diagnostic: snapshot right after TTS finishes but
            // before we decide whether to recurse for a follow-up
            // turn. This is the row that pairs with the
            // "before_capture_*" row above to confirm "yes, the
            // device really did disappear between TTS end and
            // the next capture open".
            voice::audio_devices::log_runtime_device_state("after_tts_response");

            expect_followup = self.classifier.expects_followup(&response);
            info!(
                "[voice loop] follow-up classifier => {}",
                if expect_followup { "expects user reply" } else { "session done" }
            );
            if !expect_followup {
                return Ok(());
            }
        }
    }

    /// Run `agent.run_single` while watching the wake detector in
    /// parallel. Each time the wake detector fires, play
    /// `cfg.busy_prompt` and re-arm the detector for the next
    /// potential interrupt. Returns the agent's final response (or
    /// the error/timeout TTS we synthesised when the agent
    /// itself failed).
    async fn run_agent_with_busy_intercept(
        &mut self,
        user_text: &str,
    ) -> Result<String> {
        // `agent.run_single` returns a future that's not Unpin by
        // default; pin it on the heap so we can poll it from inside
        // a `select!` and still hold onto it across loop iterations.
        let mut agent_fut: Pin<Box<dyn Future<Output = anyhow::Result<String>> + Send>> =
            Box::pin(self.agent.run_single(user_text));
        // First wake-detection future. Replaced via `Pin::set` on
        // every busy-prompt hit so the user can stack multiple
        // acknowledgements. Both futures see the same shutdown flag
        // so Ctrl+C still tears them down promptly.
        let mut wake_fut: Pin<Box<dyn Future<Output = Result<WakeEvent>> + Send>> =
            Box::pin(self.wake.wait_for_wake(self.shutdown.clone()));

        let llm_started = Instant::now();
        loop {
            tokio::select! {
                // Bias the select so a completed agent turn wins
                // ties: a wake that fires the same instant the
                // agent returns shouldn't trigger a redundant
                // "正在执行任务" right after the agent's reply
                // starts playing.
                biased;
                result = &mut agent_fut => {
                    match result {
                        Ok(r) => return Ok(r),
                        Err(e) => {
                            let msg = format!("处理出错:{}", e);
                            warn!(
                                "[voice loop] agent error after {:.2}s: {}",
                                llm_started.elapsed().as_secs_f32(),
                                e
                            );
                            return Ok(msg);
                        }
                    }
                }
                wake_res = &mut wake_fut => {
                    match wake_res {
                        Ok(event) => info!(
                            "[voice loop] wake {:?} fired during agent turn, playing busy prompt",
                            event.keyword
                        ),
                        Err(e) => {
                            // Wake detector errored — usually shutdown.
                            // Don't let a noisy wake detector abort the
                            // agent turn; just skip the busy prompt
                            // and let the agent finish.
                            warn!(
                                "[voice loop] wake detector errored during agent turn: {}",
                                e
                            );
                            if self.shutdown.load(Ordering::SeqCst) {
                                // Mirror `run` semantics: shutdown
                                // -> bail the whole loop.
                                return Err(e);
                            }
                        }
                    }
                    if !self.cfg.busy_prompt.is_empty() {
                        if let Err(e) = self
                            .tts
                            .speak(&self.cfg.busy_prompt, self.shutdown.clone())
                            .await
                        {
                            warn!("[voice loop] busy prompt TTS failed: {}", e);
                        }
                    }
                    // Re-arm the wake future. The old one is dropped
                    // here; for the sherpa KWS the inner blocking
                    // task has already returned (the wake event was
                    // what unblocked us), so dropping the future
                    // only drops an already-completed JoinHandle.
                    // `Pin::set` doesn't apply here because the
                    // value type (`dyn Future`) is not `Unpin`; the
                    // owned `mem::replace` rebuilds the Pin with a
                    // fresh Box::pin and triggers the unsized
                    // coercion from `impl Future` to `dyn Future`.
                    let new_fut: Pin<Box<dyn Future<Output = Result<WakeEvent>> + Send>> =
                        Box::pin(self.wake.wait_for_wake(self.shutdown.clone()));
                    let _ = std::mem::replace(&mut wake_fut, new_fut);
                }
            }
        }
    }

    /// Capture mic audio with a hard wall-clock timeout, then run ASR.
    /// Returns `Ok(None)` for: timeout elapsed, captured zero samples,
    /// or transcribed empty text. Otherwise returns the transcript.
    async fn capture_and_recognize(&self, listen_timeout: Duration) -> Result<Option<String>> {
        let cap = self.capture.clone();
        let asr = self.asr.clone();
        let shutdown = self.shutdown.clone();
        let work = async move {
            let samples = cap
                .capture_until_silence(listen_timeout, shutdown.clone())
                .await?;
            if samples.is_empty() {
                return Ok::<String, anyhow::Error>(String::new());
            }
            let text = asr.transcribe(&samples).await?;
            Ok(text)
        };

        match timeout(listen_timeout, work).await {
            Err(_) => {
                warn!("[voice loop] capture wall-clock timeout after {:?}", listen_timeout);
                Ok(None)
            }
            Ok(Err(e)) => {
                // Diagnostic: snapshot at the moment the
                // capture failed. The CpalCapture path opens
                // the input device inside
                // `capture_until_silence`; if cpal can't find
                // it the error bubbles up here. Compare this
                // row to the matching "before_capture_*" row
                // (printed in `run_session`) to see whether
                // cpal dropped the device between the two
                // snapshots or whether it was missing all
                // along.
                voice::audio_devices::log_runtime_device_state("after_capture_error");
                Err(e)
            }
            Ok(Ok(text)) => {
                let trimmed = text.trim().to_string();
                if trimmed.is_empty() {
                    warn!("[voice loop] ASR returned empty text => treated as timeout");
                    Ok(None)
                } else {
                    Ok(Some(trimmed))
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::atomic::AtomicUsize;
    use std::sync::Mutex;

    #[test]
    fn classifier_matches_question_mark() {
        let c = FollowupClassifier::new(&[r"[?？]\s*$".into()]);
        assert!(c.expects_followup("你确认要删除吗?"));
        assert!(c.expects_followup("是这样吗?"));
        assert!(!c.expects_followup("当前有 3 个客户端在线。"));
    }

    #[test]
    fn classifier_matches_keyword() {
        let c = FollowupClassifier::new(&["请确认".into(), "请提供".into()]);
        assert!(c.expects_followup("请确认操作"));
        assert!(c.expects_followup("请提供 client_id"));
        assert!(!c.expects_followup("操作完成。"));
    }

    #[test]
    fn classifier_ignores_empty() {
        let c = FollowupClassifier::new(&[r"[?？]\s*$".into()]);
        assert!(!c.expects_followup(""));
        assert!(!c.expects_followup("   "));
    }

    struct ImmediateCapture;
    #[async_trait]
    impl AudioCapture for ImmediateCapture {
        async fn capture_until_silence(
            &self,
            _timeout: Duration,
            _shutdown: Arc<AtomicBool>,
        ) -> anyhow::Result<Vec<f32>> {
            Ok(vec![0.0; 16])
        }
    }

    struct EmptyCapture;
    #[async_trait]
    impl AudioCapture for EmptyCapture {
        async fn capture_until_silence(
            &self,
            _timeout: Duration,
            _shutdown: Arc<AtomicBool>,
        ) -> anyhow::Result<Vec<f32>> {
            Ok(Vec::new())
        }
    }

    struct SlowCapture;
    #[async_trait]
    impl AudioCapture for SlowCapture {
        async fn capture_until_silence(
            &self,
            timeout_hint: Duration,
            _shutdown: Arc<AtomicBool>,
        ) -> anyhow::Result<Vec<f32>> {
            tokio::time::sleep(timeout_hint * 5).await;
            Ok(vec![0.0; 16])
        }
    }

    struct CannedAsr(&'static str);
    #[async_trait]
    impl AsrTranscriber for CannedAsr {
        async fn transcribe(&self, _samples: &[f32]) -> anyhow::Result<String> {
            Ok(self.0.to_string())
        }
    }

    struct RecordingTts(Arc<Mutex<Vec<String>>>);
    #[async_trait]
    impl TtsSpeaker for RecordingTts {
        async fn speak(
            &self,
            text: &str,
            _shutdown: Arc<AtomicBool>,
        ) -> anyhow::Result<()> {
            self.0.lock().unwrap().push(text.to_string());
            Ok(())
        }
    }

    struct NeverWake(Arc<AtomicBool>);
    #[async_trait]
    impl WakeDetector for NeverWake {
        async fn wait_for_wake(
            &self,
            shutdown: Arc<AtomicBool>,
        ) -> anyhow::Result<voice::WakeEvent> {
            // Spin on shutdown; if the flag flips (or the caller
            // pre-set it via the constructor), bail out cleanly so
            // VoiceLoop::run can exercise the shutdown path. This
            // mirrors the real SherpaKws behavior.
            if shutdown.load(Ordering::SeqCst) || self.0.load(Ordering::SeqCst) {
                anyhow::bail!("never-wake shutdown requested");
            }
            loop {
                if shutdown.load(Ordering::SeqCst) || self.0.load(Ordering::SeqCst) {
                    anyhow::bail!("never-wake shutdown requested");
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
    }

    fn build_loop(
        capture: Arc<dyn AudioCapture>,
        asr: Arc<dyn AsrTranscriber>,
        spoken: Arc<Mutex<Vec<String>>>,
    ) -> VoiceLoop {
        build_loop_with_shutdown(capture, asr, spoken, Arc::new(AtomicBool::new(false)))
    }

    fn build_loop_with_shutdown(
        capture: Arc<dyn AudioCapture>,
        asr: Arc<dyn AsrTranscriber>,
        spoken: Arc<Mutex<Vec<String>>>,
        shutdown: Arc<AtomicBool>,
    ) -> VoiceLoop {
        // Provide a minimal Agent isn't possible without provider/etc.,
        // so we only exercise capture_and_recognize via direct calls.
        // For that we just need the traits; agent is unused.
        let agent = AgentBuilder::new()
            .provider(Box::new(DummyProvider))
            .tools(Vec::new())
            .memory(Arc::new(zeroclaw::memory::NoneMemory::new()))
            .observer(Arc::new(zeroclaw::observability::NoopObserver))
            .tool_dispatcher(Box::new(zeroclaw::agent::dispatcher::NativeToolDispatcher))
            .model_name("test".into())
            .temperature(0.0)
            .build()
            .expect("agent build");

        VoiceLoop::new(
            agent,
            Arc::new(NeverWake(shutdown.clone())),
            capture,
            asr,
            Arc::new(RecordingTts(spoken)),
            FollowupClassifier::new(&[]),
            VoiceLoopConfig {
                capture_timeout: Duration::from_millis(200),
                followup_timeout: Duration::from_millis(200),
                wake_prompt: "在呢".into(),
                busy_prompt: "正在执行任务".into(),
                timeout_prompt: "等待超时".into(),
                exit_wake_words: Vec::new(),
                exit_prompt: None,
            },
            shutdown,
        )
    }

    struct DummyProvider;
    #[async_trait]
    impl zeroclaw::providers::Provider for DummyProvider {
        async fn chat_with_system(
            &self,
            _s: Option<&str>,
            _m: &str,
            _model: &str,
            _t: f64,
        ) -> anyhow::Result<String> {
            Ok("ok".into())
        }

        async fn chat(
            &self,
            _r: zeroclaw::providers::ChatRequest<'_>,
            _model: &str,
            _t: f64,
        ) -> anyhow::Result<zeroclaw::providers::ChatResponse> {
            Ok(zeroclaw::providers::ChatResponse {
                text: Some("ok".into()),
                tool_calls: vec![],
                usage: None,
                reasoning_content: None,
            })
        }
    }

    use zeroclaw::agent::AgentBuilder;

    #[tokio::test]
    async fn capture_returns_text_when_asr_succeeds() {
        let spoken = Arc::new(Mutex::new(Vec::new()));
        let v = build_loop(
            Arc::new(ImmediateCapture),
            Arc::new(CannedAsr("hello world")),
            spoken,
        );
        let got = v
            .capture_and_recognize(Duration::from_secs(1))
            .await
            .unwrap();
        assert_eq!(got.as_deref(), Some("hello world"));
    }

    #[tokio::test]
    async fn capture_timeout_when_asr_returns_blank() {
        let spoken = Arc::new(Mutex::new(Vec::new()));
        let v = build_loop(
            Arc::new(ImmediateCapture),
            Arc::new(CannedAsr("   ")),
            spoken,
        );
        let got = v
            .capture_and_recognize(Duration::from_secs(1))
            .await
            .unwrap();
        assert!(got.is_none());
    }

    #[tokio::test]
    async fn capture_timeout_when_samples_empty() {
        let spoken = Arc::new(Mutex::new(Vec::new()));
        let v = build_loop(
            Arc::new(EmptyCapture),
            Arc::new(CannedAsr("ignored")),
            spoken,
        );
        let got = v
            .capture_and_recognize(Duration::from_secs(1))
            .await
            .unwrap();
        assert!(got.is_none());
    }

    #[tokio::test]
    async fn capture_timeout_when_wall_clock_exceeded() {
        let spoken = Arc::new(Mutex::new(Vec::new()));
        let v = build_loop(
            Arc::new(SlowCapture),
            Arc::new(CannedAsr("never returned")),
            spoken,
        );
        let got = v
            .capture_and_recognize(Duration::from_millis(50))
            .await
            .unwrap();
        assert!(got.is_none());
    }

    /// When the shutdown flag is flipped, the wake detector (mock
    /// `NeverWake`) returns `Err`, and `VoiceLoop::run` must
    /// translate that into a clean `Ok(())` exit instead of
    /// propagating the error. This is the path Ctrl+C takes in
    /// production: the main-loop handler sets the flag, the KWS
    /// consumer loop sees it and bails, and the orchestrator
    /// unwinds.
    #[tokio::test]
    async fn run_exits_cleanly_when_shutdown_flag_set_during_wake_wait() {
        let spoken = Arc::new(Mutex::new(Vec::new()));
        let shutdown = Arc::new(AtomicBool::new(false));
        let mut v = build_loop_with_shutdown(
            Arc::new(EmptyCapture),
            Arc::new(CannedAsr("")),
            spoken,
            shutdown.clone(),
        );

        let handle = tokio::spawn(async move { v.run().await });
        // Give the orchestrator a moment to enter `wait_for_wake`.
        tokio::time::sleep(Duration::from_millis(30)).await;
        shutdown.store(true, Ordering::SeqCst);

        // `run` should return Ok within a few hundred ms — the
        // wake detector's spin loop wakes every 10 ms.
        let result =
            tokio::time::timeout(Duration::from_secs(2), handle).await;
        let joined = result
            .expect("run did not return within 2s after shutdown")
            .expect("run task panicked");
        assert!(
            joined.is_ok(),
            "shutdown-induced wake Err should map to Ok(()) but got {joined:?}"
        );
    }

    /// Setting the shutdown flag before `run` is even called must
    /// also exit cleanly — covers the case where Ctrl+C is
    /// delivered during startup.
    #[tokio::test]
    async fn run_exits_cleanly_when_shutdown_flag_already_set() {
        let spoken = Arc::new(Mutex::new(Vec::new()));
        let shutdown = Arc::new(AtomicBool::new(true));
        let mut v = build_loop_with_shutdown(
            Arc::new(EmptyCapture),
            Arc::new(CannedAsr("")),
            spoken,
            shutdown,
        );
        let result = tokio::time::timeout(Duration::from_secs(1), v.run())
            .await
            .expect("run did not return within 1s when shutdown pre-set");
        assert!(result.is_ok());
    }

    /// Regression for the "process exits with `Error: audio_input_device
    /// not found` after a flaky mic" bug. The KWS bails with an
    /// `Err(_)` when it can't open the configured input device (the
    /// most common cause is a USB mic that briefly disappears from
    /// cpal/PipeWire's view mid-session — the ALSA-sees-the-device
    /// -but-cpal-doesn't quirk). The pre-fix code returned that
    /// `Err` from `run()`, which propagated to `main()` and killed
    /// the process. The fix is to treat a non-shutdown wake-detector
    /// error as a transient failure: log a warning, sleep 1 s, and
    /// re-enter `wait_for_wake` so the loop survives a flaky mic
    /// and recovers automatically when the device comes back.
    ///
    /// This test wires up a wake detector that errors twice, then
    /// fires a real wake on the third call, then blocks on shutdown
    /// (so a stray retry doesn't accidentally fire again and
    /// re-trigger the session). Asserts the loop actually ran the
    /// session (proving it didn't exit after the first two errors).
    #[tokio::test]
    async fn run_retries_when_wake_detector_errors_transiently() {
        use std::sync::atomic::AtomicUsize;
        struct FlakeyWake {
            fired: Arc<AtomicUsize>,
        }
        #[async_trait]
        impl WakeDetector for FlakeyWake {
            async fn wait_for_wake(
                &self,
                shutdown: Arc<AtomicBool>,
            ) -> anyhow::Result<voice::WakeEvent> {
                let n = self.fired.fetch_add(1, Ordering::SeqCst);
                match n {
                    0..=1 => {
                        // Simulate a transient device-open failure
                        // (USB mic disappeared from cpal's view
                        // mid-session). The orchestrator should
                        // NOT exit on this — it should log, sleep
                        // 1 s, and retry.
                        anyhow::bail!("audio_input_device \"USB MIC Device\" not found (simulated)");
                    }
                    2 => {
                        // Third attempt: the "device came back" —
                        // fire a real wake.
                        Ok(WakeEvent::wake("recovered"))
                    }
                    _ => {
                        // Subsequent calls (post-recovery): block
                        // on shutdown so the loop's next
                        // `wait_for_wake` after the session
                        // returns doesn't re-fire the wake and
                        // re-enter the session, which would make
                        // the test hang waiting for shutdown.
                        if shutdown.load(Ordering::SeqCst) {
                            anyhow::bail!("flakey wake shutdown requested");
                        }
                        loop {
                            if shutdown.load(Ordering::SeqCst) {
                                anyhow::bail!("flakey wake shutdown requested");
                            }
                            tokio::time::sleep(Duration::from_millis(10)).await;
                        }
                    }
                }
            }
        }

        let spoken = Arc::new(Mutex::new(Vec::<String>::new()));
        let shutdown = Arc::new(AtomicBool::new(false));
        let fired = Arc::new(AtomicUsize::new(0));
        let wake: Arc<dyn WakeDetector> = Arc::new(FlakeyWake {
            fired: fired.clone(),
        });
        let mut v = build_loop_with_provider(
            Arc::new(ImmediateCapture),
            Arc::new(CannedAsr("hello agent")),
            spoken.clone(),
            wake,
            shutdown.clone(),
            SlowProvider {
                delay: Duration::from_millis(20),
            },
        );

        let handle = tokio::spawn(async move { v.run().await });
        // The first two attempts error, the loop sleeps 1 s between
        // each (so ~2 s of sleep total). The third attempt fires
        // the wake, the session runs (ImmediateCapture returns 16
        // samples instantly, ASR returns "hello agent", agent
        // sleeps 20 ms, then the response is played). After the
        // session the next `wait_for_wake` blocks on shutdown.
        tokio::time::sleep(Duration::from_millis(2300)).await;
        shutdown.store(true, Ordering::SeqCst);

        let result = tokio::time::timeout(Duration::from_secs(2), handle)
            .await
            .expect("run did not return within 2s after shutdown");
        let joined = result.expect("run task panicked");
        assert!(
            joined.is_ok(),
            "run() should return Ok after transient wake errors, got {joined:?}"
        );
        // The wake detector should have been polled at least 4
        // times: 2 errors + the successful third that fired the
        // session + the post-session call that blocks on shutdown.
        assert!(
            fired.load(Ordering::SeqCst) >= 4,
            "expected ≥4 wake attempts (2 errors + 1 success + 1 post-session), got {}",
            fired.load(Ordering::SeqCst)
        );
        // And the session should have actually run after the
        // recovery — the spoken queue should contain "在呢" (the
        // wake_prompt).
        let spoken_log = spoken.lock().unwrap().clone();
        assert!(
            spoken_log.iter().any(|s| s == "在呢"),
            "wake_prompt should have been spoken after the retry succeeded; spoken = {spoken_log:?}"
        );
    }

    /// Capture path shutdown: `capture_and_recognize` uses
    /// `AudioCapture::capture_until_silence` on a `spawn_blocking`
    /// task. When the shutdown flag flips, the mock `EmptyCapture`
    /// bails immediately (returning `Vec::new()`), the orchestrator
    /// treats that as a timeout, and `run_session` returns
    /// `Ok(())` cleanly. Verifies that the
    /// Ctrl+C-during-recording path doesn't leave the process
    /// blocked on the capture timeout (30 s by default).
    #[tokio::test]
    async fn capture_path_returns_quickly_when_shutdown_set_mid_capture() {
        use std::sync::atomic::Ordering;
        // A capture mock that respects the shutdown flag — mirrors
        // the real `CpalCapture` behavior.
        struct ShutdownAwareCapture(Arc<AtomicBool>);
        #[async_trait]
        impl AudioCapture for ShutdownAwareCapture {
            async fn capture_until_silence(
                &self,
                _timeout: Duration,
                shutdown: Arc<AtomicBool>,
            ) -> anyhow::Result<Vec<f32>> {
                // If pre-set, bail. Otherwise wait for the flag.
                if shutdown.load(Ordering::SeqCst) || self.0.load(Ordering::SeqCst) {
                    return Ok(Vec::new());
                }
                loop {
                    if shutdown.load(Ordering::SeqCst) || self.0.load(Ordering::SeqCst) {
                        return Ok(Vec::new());
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }
        }

        let spoken = Arc::new(Mutex::new(Vec::new()));
        let shutdown = Arc::new(AtomicBool::new(false));
        let mut v = build_loop_with_shutdown(
            Arc::new(ShutdownAwareCapture(shutdown.clone())),
            Arc::new(CannedAsr("")),
            spoken,
            shutdown.clone(),
        );

        // Flip the flag almost immediately and confirm run() unwinds.
        let s2 = shutdown.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            s2.store(true, Ordering::SeqCst);
        });
        let result = tokio::time::timeout(Duration::from_secs(2), v.run())
            .await
            .expect("run did not return within 2s after capture-path shutdown");
        assert!(result.is_ok());
    }

    /// A provider that sleeps for `delay` before returning, so we
    /// can race a wake future against a still-running agent turn
    /// and observe the busy-prompt branch.
    struct SlowProvider {
        delay: Duration,
    }
    #[async_trait]
    impl zeroclaw::providers::Provider for SlowProvider {
        async fn chat_with_system(
            &self,
            _s: Option<&str>,
            _m: &str,
            _model: &str,
            _t: f64,
        ) -> anyhow::Result<String> {
            tokio::time::sleep(self.delay).await;
            Ok("agent done".into())
        }
        async fn chat(
            &self,
            _r: zeroclaw::providers::ChatRequest<'_>,
            _model: &str,
            _t: f64,
        ) -> anyhow::Result<zeroclaw::providers::ChatResponse> {
            tokio::time::sleep(self.delay).await;
            Ok(zeroclaw::providers::ChatResponse {
                text: Some("agent done".into()),
                tool_calls: vec![],
                usage: None,
                reasoning_content: None,
            })
        }
    }

    fn build_loop_with_provider<P: zeroclaw::providers::Provider + 'static>(
        capture: Arc<dyn AudioCapture>,
        asr: Arc<dyn AsrTranscriber>,
        spoken: Arc<Mutex<Vec<String>>>,
        wake: Arc<dyn WakeDetector>,
        shutdown: Arc<AtomicBool>,
        provider: P,
    ) -> VoiceLoop {
        let agent = AgentBuilder::new()
            .provider(Box::new(provider))
            .tools(Vec::new())
            .memory(Arc::new(zeroclaw::memory::NoneMemory::new()))
            .observer(Arc::new(zeroclaw::observability::NoopObserver))
            .tool_dispatcher(Box::new(zeroclaw::agent::dispatcher::NativeToolDispatcher))
            .model_name("test".into())
            .temperature(0.0)
            .build()
            .expect("agent build");

        VoiceLoop::new(
            agent,
            wake,
            capture,
            asr,
            Arc::new(RecordingTts(spoken)),
            FollowupClassifier::new(&[]),
            VoiceLoopConfig {
                capture_timeout: Duration::from_millis(200),
                followup_timeout: Duration::from_millis(200),
                wake_prompt: "在呢".into(),
                busy_prompt: "正在执行任务".into(),
                timeout_prompt: "等待超时".into(),
                exit_wake_words: Vec::new(),
                exit_prompt: None,
            },
            shutdown,
        )
    }

    /// End-to-end check of the wake → agent → busy-prompt flow:
    ///   1. Wake fires → orchestrator plays "在呢" before opening mic.
    ///   2. Capture returns user text → agent starts (slow, 200 ms).
    ///   3. Wake fires again *during* the agent turn → orchestrator
    ///      plays "正在执行任务" but lets the agent finish.
    ///   4. Agent returns → orchestrator plays the response.
    ///   5. `run` is shut down so the test exits.
    ///
    /// Verifies the order and content of the spoken queue.
    #[tokio::test]
    async fn busy_prompt_fires_when_wake_interrupts_agent_turn() {
        let spoken = Arc::new(Mutex::new(Vec::<String>::new()));
        let shutdown = Arc::new(AtomicBool::new(false));

        // Two-phase scripted wake: first `wait_for_wake` call
        // returns immediately (starts the session), the second
        // waits 100 ms (so the agent has actually started running
        // when it fires) and then returns. Subsequent calls block
        // on shutdown so the test exits cleanly. This mirrors what
        // a real KWS does when the user re-says the wake word
        // during a long LLM turn.
        struct PhasedScriptedWake {
            fired_count: Arc<AtomicUsize>,
        }
        #[async_trait]
        impl WakeDetector for PhasedScriptedWake {
            async fn wait_for_wake(
                &self,
                shutdown: Arc<AtomicBool>,
            ) -> anyhow::Result<voice::WakeEvent> {
                let n = self.fired_count.fetch_add(1, Ordering::SeqCst);
                match n {
                    0 => Ok(WakeEvent::wake("phased")), // initial session wake
                    1 => {
                        // mid-agent wake: let the agent start, then fire
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        if shutdown.load(Ordering::SeqCst) {
                            anyhow::bail!("phased wake shutdown requested");
                        }
                        Ok(WakeEvent::wake("phased"))
                    }
                    _ => {
                        // Subsequent calls: block until shutdown.
                        loop {
                            if shutdown.load(Ordering::SeqCst) {
                                anyhow::bail!("phased wake shutdown requested");
                            }
                            tokio::time::sleep(Duration::from_millis(10)).await;
                        }
                    }
                }
            }
        }

        let wake: Arc<dyn WakeDetector> = Arc::new(PhasedScriptedWake {
            fired_count: Arc::new(AtomicUsize::new(0)),
        });

        let mut v = build_loop_with_provider(
            Arc::new(ImmediateCapture),
            Arc::new(CannedAsr("hello agent")),
            spoken.clone(),
            wake,
            shutdown.clone(),
            SlowProvider {
                delay: Duration::from_millis(300),
            },
        );

        // Shut down well after the agent has finished (300 ms) and
        // after a possible second wake session has started so we
        // exit cleanly. 800 ms gives enough slack: 1st wake →
        // 在呢 (instant) → capture (instant) → agent (300 ms) → 2nd
        // wake fires at ~100 ms into the agent turn → busy prompt
        // → agent finishes → "agent done" spoken → session ends
        // (no follow-up) → 3rd `wait_for_wake` blocks → we flip
        // shutdown.
        let s2 = shutdown.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(800)).await;
            s2.store(true, Ordering::SeqCst);
        });

        let result = tokio::time::timeout(Duration::from_secs(3), v.run()).await;
        let _ = result.expect("run did not return within 3s");

        let queue = spoken.lock().unwrap().clone();
        // We expect at minimum: ["在呢", "正在执行任务", "agent done"].
        // The exact tail depends on shutdown timing; we only assert
        // the prefix here.
        assert!(
            queue.windows(3).any(|w| w == ["在呢", "正在执行任务", "agent done"]),
            "expected ['在呢', '正在执行任务', 'agent done'] in order, got {:?}",
            queue,
        );
        // The agent response must come *after* the busy prompt
        // (i.e. the agent was allowed to finish even though a wake
        // arrived during the turn).
        let busy_idx = queue
            .iter()
            .position(|s| s == "正在执行任务")
            .expect("busy prompt not spoken");
        let agent_idx = queue
            .iter()
            .position(|s| s == "agent done")
            .expect("agent response not spoken");
        assert!(
            agent_idx > busy_idx,
            "agent response should come after busy prompt (got queue {:?})",
            queue
        );
        // The initial "在呢" must come first — the wake
        // acknowledgement is always spoken before capture starts.
        assert_eq!(queue[0], "在呢", "first prompt should be the wake acknowledgement, got {:?}", queue);
    }
}
