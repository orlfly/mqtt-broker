//! The merged binary.
//!
//! Starts the MQTT broker, the (optional) HTTP management
//! API, the in-process management channel, and the voice-
//! loop agent — all in one tokio runtime. The agent's
//! `zeroclaw` tools are wired directly to the broker via
//! the management channel, so no HTTP hop is involved when
//! the LLM asks "list the connected clients".
//!
//! Lifecycle (Ctrl+C shuts the whole thing down):
//!
//! ```text
//!   ┌──────────────┐
//!   │ MqttEngine   │──── accept loop on 1883 ──────────────► MQTT
//!   │   state      │
//!   │   mgmt task ─┼──── mpsc/oneshot ──► agent tools
//!   └──────────────┘
//!   ┌──────────────┐
//!   │ HTTP API     │──── axum on 8080  ───────────────────► admin tools
//!   │   (optional) │
//!   └──────────────┘
//!   ┌──────────────┐
//!   │ VoiceLoop    │──── wake → capture → agent → TTS ────► mic / speaker
//!   └──────────────┘
//! ```

use std::sync::Arc;
use std::time::Duration;

use agent::{
    FollowupClassifier, GetTopicSubscribersTool, ListClientsTool, ListTopicsTool,
    VoiceLoop, VoiceLoopConfig,
};
use anyhow::Result;
use api::auth::JwtAuth;
use api::create_router;
use broker::MqttEngine;
use common::Config;
use tokio::net::TcpListener;
use tracing::{error, info};
use tracing_subscriber::EnvFilter;
use zeroclaw::agent::dispatcher::NativeToolDispatcher;
use zeroclaw::agent::AgentBuilder;
use zeroclaw::memory::NoneMemory;
use zeroclaw::observability::NoopObserver;
use zeroclaw::providers::compatible::{AuthStyle, OpenAiCompatibleProvider};
use zeroclaw::security::{AutonomyLevel, SecurityPolicy};

mod soul;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let cfg = Config::load_default()?;
    info!(
        "Starting unified app (mqtt {}, api {}, voice={})",
        cfg.mqtt_bind_addr(),
        cfg.api_bind_addr(),
        cfg.agent.channels.voice.engine
    );

    // ---- 1. MQTT broker ------------------------------------------------
    let engine = Arc::new(MqttEngine::new());
    let broker_state = engine.state();

    let mqtt_addr = cfg.mqtt_bind_addr();
    let mqtt_engine = engine.clone();
    let mqtt_handle = tokio::spawn(async move {
        if let Err(e) = mqtt_engine.start(&mqtt_addr).await {
            error!("MQTT engine stopped with error: {}", e);
        }
    });

    // ---- 2. In-process management channel -----------------------------
    // The management task is the only place that holds the
    // broker's read lock for management queries. Tools and
    // the HTTP API both go through this channel. The HTTP
    // path could in principle take the lock directly, but
    // routing everything through the channel gives a
    // uniform trace + a single contention point.
    let (mgmt, mgmt_task) = engine.management_pair();
    tokio::spawn(mgmt_task);

    // ---- 3. HTTP management API (optional, for external admin tools) -
    // Kept around for backward compatibility with any
    // existing admin scripts. The agent itself never calls
    // HTTP — it uses the `mgmt` handle directly.
    if cfg.api.enabled {
        let jwt_auth = Arc::new(JwtAuth::new(
            cfg.api.token.secret.clone(),
            cfg.api.token.expire_secs,
        ));
        let router = create_router(broker_state.clone(), jwt_auth);
        let api_addr = cfg.api_bind_addr();
        let api_listener = TcpListener::bind(&api_addr).await?;
        info!("REST API listening on {}", api_addr);
        tokio::spawn(async move {
            if let Err(e) = axum::serve(api_listener, router).await {
                error!("API server stopped with error: {}", e);
            }
        });
    } else {
        info!("HTTP management API disabled (api.enabled = false)");
    }

    // ---- 4. Agent + voice loop -----------------------------------------
    let voice_cfg = &cfg.agent.channels.voice;
    if !voice_cfg.enabled {
        anyhow::bail!(
            "voice channel is disabled in config; this build exposes the voice channel \
             (set agent.channels.voice.enabled = true in broker.yaml)"
        );
    }

    // Voice stack: KWS + capture + ASR + TTS. Built by the
    // voice crate, same code path as before.
    let stack = voice::build_stack(voice_cfg)?;
    info!(
        "Voice engine={}, sample_rate={}, label='{}'",
        voice_cfg.engine, voice_cfg.sample_rate, voice_cfg.wake_word
    );

    if voice_cfg.engine == "sherpa" {
        voice::audio_devices::log_audio_devices();
        voice::audio_devices::log_diagnostic_commands();
        if let Some(server) = voice::audio_devices::detect_sound_server() {
            info!("[audio devices] sound server: {}", server);
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

    // ---- 5. Tools — wired to the in-process management channel ---------
    // Each tool holds a cheap clone of the handle. The
    // handle internally is an `mpsc::Sender` so cloning is
    // a single `Arc` bump.

    // Resolve workspace dir early — needed for both soul seeding and
    // zeroclaw file tool sandboxing.
    let workspace_dir = soul::resolve_workspace_dir(&cfg.agent.workspace_dir);

    // Build zeroclaw file management tools (shell, file read/write/edit,
    // glob search, content search) with Full autonomy within workspace.
    let security = Arc::new(SecurityPolicy {
        autonomy: AutonomyLevel::Full,
        workspace_dir: workspace_dir.clone(),
        workspace_only: true,
        ..SecurityPolicy::default()
    });
    let file_tools = zeroclaw::tools::default_tools(security);

    let mut tools: Vec<Box<dyn zeroclaw::tools::Tool>> = vec![
        Box::new(ListClientsTool::new(mgmt.clone())),
        Box::new(ListTopicsTool::new(mgmt.clone())),
        Box::new(GetTopicSubscribersTool::new(mgmt.clone())),
    ];
    tools.extend(file_tools);

    // ---- 5b. OpenClaw soul workspace ----------------------------------
    // `zeroclaw` 的 system-prompt 拼装会按固定顺序读工作区里的
    // AGENTS.md / SOUL.md / TOOLS.md / IDENTITY.md / USER.md 等。
    // 这里：解析路径 → 首次启动写默认内容 → 把绝对路径和
    // identity_format 透传给 AgentBuilder。
    soul::ensure_workspace(&workspace_dir)?;
    let identity_config = soul::build_identity_config(&cfg.agent.identity_format);

    let agent = build_agent(&cfg, tools, workspace_dir, identity_config)?;
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
        voice_output: cfg.agent.voice_output.clone(),
    };

    let shutdown = Arc::new(std::sync::atomic::AtomicBool::new(false));
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
    voice::audio_devices::log_runtime_device_state("startup_baseline");

    // ---- 6. Ctrl+C -----------------------------------------------------
    let signal_shutdown = shutdown.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            info!("Received Ctrl+C, signaling shutdown");
            signal_shutdown.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    });

    // ---- 7. Run ---------------------------------------------------------
    // The voice loop is the foreground task. When it
    // returns (Ctrl+C → shutdown flag → wake detector
    // errors → loop unwinds), the broker and HTTP tasks are
    // dropped along with the runtime.
    let result = voice.run().await;

    // Make sure we see the management task's exit line
    // before main returns. Not strictly necessary, but
    // helpful for "did everything shut down cleanly?"
    // diagnostics.
    drop(mgmt); // release the last handle clone
    let _ = tokio::time::timeout(Duration::from_millis(200), mqtt_handle).await;
    result
}

fn build_agent(
    cfg: &Config,
    tools: Vec<Box<dyn zeroclaw::tools::Tool>>,
    workspace_dir: std::path::PathBuf,
    identity_config: zeroclaw::config::IdentityConfig,
) -> Result<zeroclaw::agent::Agent> {
    let llm_cfg = &cfg.agent.llm;
    let api_key = llm_cfg.api_key.clone().unwrap_or_else(|| "ollama".into());
    let provider = OpenAiCompatibleProvider::new(
        "ollama",
        &llm_cfg.api_url,
        Some(&api_key),
        AuthStyle::Bearer,
    );

    let agent = AgentBuilder::new()
        .provider(Box::new(provider))
        .tools(tools)
        .memory(Arc::new(NoneMemory::new()))
        .observer(Arc::new(NoopObserver))
        .tool_dispatcher(Box::new(NativeToolDispatcher))
        .model_name(llm_cfg.model.clone())
        .temperature(llm_cfg.temperature)
        .workspace_dir(workspace_dir)
        .identity_config(identity_config)
        .build()?;

    Ok(agent)
}
