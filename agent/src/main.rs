use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use common::Config;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;
use zeroclaw::agent::AgentBuilder;
use zeroclaw::agent::dispatcher::NativeToolDispatcher;
use zeroclaw::memory::NoneMemory;
use zeroclaw::observability::NoopObserver;
use zeroclaw::providers::compatible::{AuthStyle, OpenAiCompatibleProvider};

mod skills;
mod voice_loop;

use voice_loop::{FollowupClassifier, VoiceLoop, VoiceLoopConfig};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Honour RUST_LOG if set, otherwise default to INFO. `fmt::init()`
    // alone defaults to ERROR in newer tracing-subscriber versions, which
    // hides all the voice-engine / voice-loop logs we just added.
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let cfg = Config::load_default()?;
    info!(
        "Starting AI Agent (model={}, llm={})",
        cfg.agent.llm.model, cfg.agent.llm.api_url
    );

    let agent = build_agent(&cfg)?;

    let voice_cfg = &cfg.agent.channels.voice;
    if !voice_cfg.enabled {
        anyhow::bail!(
            "voice channel is disabled in config; this build only exposes the voice channel \
             (set agent.channels.voice.enabled = true in broker.yaml)"
        );
    }

    let stack = voice::build_stack(voice_cfg)?;
    // In stub mode `wake_word` is the literal string the stub fires
    // on. In sherpa mode the actual wake detection is driven by
    // `kws_keywords_inline` and `wake_word` is just a display label —
    // the per-engine startup logs that follow make the distinction
    // explicit. We print it here as a sanity-check that the right
    // config was loaded.
    info!(
        "Voice engine={}, sample_rate={}, label='{}' (display only in sherpa mode — see active aliases below)",
        voice_cfg.engine, voice_cfg.sample_rate, voice_cfg.wake_word
    );

    if voice_cfg.engine == "sherpa" {
        voice::audio_devices::log_audio_devices();
        voice::audio_devices::log_diagnostic_commands();
        if let Some(server) = voice::audio_devices::detect_sound_server() {
            info!("[audio devices] sound server: {}", server);
        } else {
            warn!("[audio devices] no PulseAudio/PipeWire server reachable via `pactl info`");
        }
        if let Some(in_dev) = voice_cfg.audio_input_device.as_deref() {
            if !in_dev.trim().is_empty() {
                voice::audio_devices::log_pulseaudio_source_state(in_dev.trim());
            }
        }
        voice::audio_devices::log_selected_devices(
            voice_cfg.audio_input_device.as_deref(),
            voice_cfg.audio_output_device.as_deref(),
        )?;
        // Optional one-shot recording for debugging the wake path.
        // Set VOICE_DUMP_WAKE_TEST=/tmp/wake.wav cargo run -p agent
        // to record 3 s of mic audio and exit. Uses the same KWS
        // config builder as the live KWS, so the wake-test path
        // can never drift from production.
        if let Ok(path) = std::env::var("VOICE_DUMP_WAKE_TEST") {
            let secs: f32 = std::env::var("VOICE_DUMP_WAKE_SECS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(3.0);
            let kws_cfg =
                voice::sherpa_kws::SherpaKws::config_from_voice_channel(voice_cfg)?;
            return voice::sherpa_kws::dump_wake_test(&kws_cfg, &path, secs);
        }
    }

    let classifier = FollowupClassifier::new(&voice_cfg.follow_up_patterns);
    let loop_cfg = VoiceLoopConfig {
        capture_timeout: Duration::from_secs(voice_cfg.capture_timeout_secs),
        followup_timeout: Duration::from_secs(voice_cfg.followup_timeout_secs),
        wake_prompt: voice_cfg.wake_prompt.clone(),
        busy_prompt: voice_cfg.busy_prompt.clone(),
        timeout_prompt: voice_cfg.timeout_prompt.clone(),
        exit_wake_words: voice_cfg.exit_wake_words.clone(),
        exit_prompt: voice_cfg
            .exit_wake_words
            .first()
            .map(|_| "好的,任务已退出".to_string()),
    };

    // Process-wide shutdown flag. The Ctrl+C handler in main() flips
    // it; the wake detector (sherpa-onnx KWS) polls it on every
    // ~10 ms tick of its inner consumer loop, so shutdown latency is
    // bounded to the size of one ringbuf pop, not the next wake
    // event. VoiceLoop::run also checks it between sessions.
    let shutdown = Arc::new(AtomicBool::new(false));

    let mut voice = VoiceLoop::new(
        agent,
        stack.wake,
        stack.capture,
        stack.asr,
        stack.tts,
        classifier,
        loop_cfg,
        shutdown.clone(),
    );

    info!(
        "Voice loop ready (capture_timeout={}s, followup_timeout={}s, exit_wake_words={:?})",
        voice_cfg.capture_timeout_secs, voice_cfg.followup_timeout_secs, voice_cfg.exit_wake_words
    );

    // Baseline device snapshot taken right before the loop starts.
    // Pair this row with the after_tts_response / after_wake_error
    // rows printed during the run to see the diff: if cpal stops
    // listing the USB mic mid-session, the gap is on the
    // PipeWire / cpal side; if pactl also stops listing it, the
    // kernel dropped the device (udev unplug, suspend, etc.).
    voice::audio_devices::log_runtime_device_state("startup_baseline");

    // Listen for Ctrl+C on a separate task and flip the shutdown
    // flag. We use a separate task (rather than `tokio::select!`)
    // so the main await is just `voice.run()` — when Ctrl+C fires,
    // the flag is set, the KWS consumer loop sees it within ~10 ms,
    // the KWS returns `Err`, the orchestrator translates that to
    // `Ok(())` (because shutdown is set), `voice.run()` returns,
    // and main() exits cleanly. No `spawn_blocking` task outlives
    // the process.
    let signal_shutdown = shutdown.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            info!("Received Ctrl+C, signaling shutdown");
            signal_shutdown.store(true, Ordering::SeqCst);
        }
    });

    voice.run().await
}

fn build_agent(cfg: &Config) -> anyhow::Result<zeroclaw::agent::Agent> {
    let llm_cfg = &cfg.agent.llm;
    let api_key = llm_cfg.api_key.clone().unwrap_or_else(|| "ollama".into());
    let provider = OpenAiCompatibleProvider::new(
        "ollama",
        &llm_cfg.api_url,
        Some(&api_key),
        AuthStyle::Bearer,
    );

    let mut tools: Vec<Box<dyn zeroclaw::tools::Tool>> = Vec::new();
    let mqtt_skill = &cfg.agent.skills.mqtt_manager;
    if mqtt_skill.enabled {
        tools.push(Box::new(skills::mqtt_manager::ListClientsTool::new(
            mqtt_skill.api_base_url.clone(),
            mqtt_skill.api_token.clone(),
        )));
        tools.push(Box::new(skills::mqtt_manager::ListTopicsTool::new(
            mqtt_skill.api_base_url.clone(),
            mqtt_skill.api_token.clone(),
        )));
        tools.push(Box::new(skills::mqtt_manager::GetTopicSubscribersTool::new(
            mqtt_skill.api_base_url.clone(),
            mqtt_skill.api_token.clone(),
        )));
    }

    let agent = AgentBuilder::new()
        .provider(Box::new(provider))
        .tools(tools)
        .memory(Arc::new(NoneMemory::new()))
        .observer(Arc::new(NoopObserver))
        .tool_dispatcher(Box::new(NativeToolDispatcher))
        .model_name(llm_cfg.model.clone())
        .temperature(llm_cfg.temperature)
        .build()?;

    Ok(agent)
}
