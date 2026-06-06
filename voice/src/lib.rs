//! ## Pipeline overview
//!
//! See `voice/docs/pipeline.md` for the full startup order,
//! device-resolution rules, wake→ASR→TTS handoff, and known
//! PipeWire quirks. Short version:
//!
//! 1. [`build_stack`] returns a [`VoiceStack`] of four
//!    `Arc<dyn Trait>` primitives, picking sherpa vs stub impls
//!    from `cfg.engine`.
//! 2. The `agent` binary then calls `audio_devices::log_*` to
//!    print a one-screen startup diagnostic (cpal list, arecord,
//!    pactl, sound-server detection, selected device's
//!    capabilities + default config).
//! 3. `VoiceLoop::run()` drives the wake → capture → ASR → LLM
//!    → TTS cycle; the loop is *not* owned by this crate.
//!
//! Engine-specific notes live in the per-file module doc comments
//! (`audio_devices`, `cpal_capture`, `sherpa_kws`, `sherpa_asr`,
//! `sherpa_tts`).

pub mod asr;
pub mod audio;
pub mod traits;
pub mod tts;
pub mod wake;

#[cfg(feature = "sherpa")]
pub mod audio_devices;
#[cfg(feature = "sherpa")]
pub mod cpal_capture;
#[cfg(feature = "sherpa")]
pub mod cpal_playback;
#[cfg(feature = "sherpa")]
pub mod sherpa_asr;
#[cfg(feature = "sherpa")]
pub mod sherpa_kws;
#[cfg(feature = "sherpa")]
pub mod sherpa_tts;

pub use asr::{AsrEngine, ScriptedAsr};
pub use audio::{AudioIO, StubAudioCapture};
pub use traits::{AsrTranscriber, AudioCapture, TtsSpeaker, WakeDetector, WakeEvent, WakeKind};
pub use tts::{StubTts, TtsEngine};
pub use wake::{ScriptedWakeDetector, StubWakeDetector};

use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Result};

/// One of each voice primitive. Returned by [`build_stack`].
pub struct VoiceStack {
    pub wake: Arc<dyn WakeDetector>,
    pub capture: Arc<dyn AudioCapture>,
    pub asr: Arc<dyn AsrTranscriber>,
    pub tts: Arc<dyn TtsSpeaker>,
}

/// Construct the four voice primitives based on `cfg.engine`.
///
/// `engine = "stub"` (default) wires up the in-process log/scripted
/// implementations. `engine = "sherpa"` wires up real cpal capture/playback
/// plus sherpa-onnx ASR/TTS/wake detection; requires the `sherpa` feature
/// on this crate.
pub fn build_stack(cfg: &common::VoiceChannelConfig) -> Result<VoiceStack> {
    match cfg.engine.as_str() {
        "" | "stub" => build_stub_stack(cfg),
        #[cfg(feature = "sherpa")]
        "sherpa" => build_sherpa_stack(cfg),
        #[cfg(not(feature = "sherpa"))]
        "sherpa" => bail!(
            "voice.engine = 'sherpa' requires rebuilding the voice crate with --features sherpa"
        ),
        other => bail!("unknown voice.engine {:?} (expected 'stub' or 'sherpa')", other),
    }
}

fn build_stub_stack(cfg: &common::VoiceChannelConfig) -> Result<VoiceStack> {
    let wake: Arc<dyn WakeDetector> = Arc::new(
        StubWakeDetector::new(
            cfg.wake_word.clone(),
            Duration::from_secs(cfg.stub_wake_interval_secs),
        )
        .with_exit_words(cfg.exit_wake_words.clone()),
    );
    let capture: Arc<dyn AudioCapture> = Arc::new(StubAudioCapture::new(
        cfg.sample_rate,
        Duration::from_millis(800),
    ));
    let utterances = if cfg.stub_utterances.is_empty() {
        vec!["你好".to_string()]
    } else {
        cfg.stub_utterances.clone()
    };
    let asr: Arc<dyn AsrTranscriber> = Arc::new(ScriptedAsr::new(utterances));
    let tts: Arc<dyn TtsSpeaker> = Arc::new(StubTts::default());
    Ok(VoiceStack { wake, capture, asr, tts })
}

#[cfg(feature = "sherpa")]
fn build_sherpa_stack(cfg: &common::VoiceChannelConfig) -> Result<VoiceStack> {
    use crate::cpal_capture::{CpalCapture, CpalCaptureConfig};
    use crate::sherpa_asr::{SherpaAsrConfig, SherpaStreamingAsr};
    use crate::sherpa_kws::SherpaKws;
    use crate::sherpa_tts::{SherpaTts, SherpaTtsConfig};

    fn required(opt: &Option<String>, name: &str) -> Result<String> {
        opt.as_ref()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("voice.{} is required for engine='sherpa'", name))
    }

    let kws_cfg = SherpaKws::config_from_voice_channel(cfg)?;
    let wake: Arc<dyn WakeDetector> = Arc::new(SherpaKws::new(kws_cfg)?);

    let asr_cfg = SherpaAsrConfig {
        encoder: required(&cfg.asr_encoder, "asr_encoder")?,
        decoder: required(&cfg.asr_decoder, "asr_decoder")?,
        joiner: required(&cfg.asr_joiner, "asr_joiner")?,
        tokens: required(&cfg.asr_tokens, "asr_tokens")?,
        num_threads: cfg.num_threads,
        provider: cfg.provider.clone(),
        debug: false,
        sample_rate: cfg.sample_rate as i32,
    };
    let asr: Arc<dyn AsrTranscriber> = Arc::new(SherpaStreamingAsr::new(asr_cfg)?);

    let cap_cfg = CpalCaptureConfig {
        device_name: cfg.audio_input_device.clone(),
        sample_rate: cfg.sample_rate,
        vad_model: cfg.vad_model.clone(),
        vad_threshold: cfg.vad_threshold,
        vad_min_silence_ms: cfg.vad_min_silence_ms,
        vad_min_speech_ms: cfg.vad_min_speech_ms,
        vad_max_speech_secs: cfg.vad_max_speech_secs,
        vad_num_threads: cfg.vad_num_threads,
        vad_buffer_secs: cfg.vad_buffer_secs,
        rms_threshold: cfg.rms_threshold,
        silence_ms: cfg.silence_ms,
        pre_speech_ms: cfg.pre_speech_ms,
    };
    let capture: Arc<dyn AudioCapture> = Arc::new(CpalCapture::new(cap_cfg));

    let tts_cfg = SherpaTtsConfig {
        model: required(&cfg.tts_model, "tts_model")?,
        tokens: required(&cfg.tts_tokens, "tts_tokens")?,
        data_dir: required(&cfg.tts_data_dir, "tts_data_dir")?,
        length_scale: cfg.tts_length_scale,
        speed: cfg.tts_speed,
        noise_scale: cfg.tts_noise_scale,
        noise_scale_w: cfg.tts_noise_scale_w,
        num_threads: cfg.num_threads,
        debug: cfg.tts_debug,
        output_device: cfg.audio_output_device.clone(),
    };
    let tts: Arc<dyn TtsSpeaker> = Arc::new(SherpaTts::new(tts_cfg)?);

    Ok(VoiceStack { wake, capture, asr, tts })
}
