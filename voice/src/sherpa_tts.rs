//! Offline TTS (sherpa-onnx) wrapped to implement `TtsSpeaker`.
//!
//! Supports three backends via `SherpaTtsConfig::backend`:
//!
//! - `"vits"` (default) — Piper-style single-speaker VITS. Mirrors
//!   `sherpa-onnx/rust-api-examples/examples/vits_tts.rs`.
//! - `"kokoro"` — Kokoro multi-speaker, multi-lang TTS. Mirrors
//!   `sherpa-onnx/rust-api-examples/examples/kokoro_tts_zh_en.rs`.
//! - `"matcha"` — Matcha-TTS (non-autoregressive, single-speaker).
//!   Mirrors `sherpa-onnx/rust-api-examples/examples/matcha_tts_zh.rs`.
//!
//! The runtime is the same in all cases: build an
//! `OfflineTtsConfig { model: OfflineTtsModelConfig { vits|kokoro|matcha: ... } }`,
//! then call `tts.generate_with_config(text, &gen_config, Some(callback))`.
//! The callback receives `(samples, progress)` chunks and returns `true`
//! to keep generating. `GeneratedAudio` holds a `!Send` raw pointer, so
//! we extract everything we need synchronously and let the handle drop
//! before `await`ing the playback task.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use async_trait::async_trait;
use sherpa_onnx::{
    GenerationConfig, OfflineTts, OfflineTtsConfig, OfflineTtsKokoroModelConfig,
    OfflineTtsMatchaModelConfig, OfflineTtsVitsModelConfig,
};
use tracing::info;

use crate::cpal_playback::play_samples;
use crate::traits::TtsSpeaker;

#[derive(Clone, Debug)]
pub struct SherpaTtsConfig {
    /// "vits" (default) | "kokoro" | "matcha".
    pub backend: String,
    // Shared
    pub model: String,
    pub tokens: String,
    pub data_dir: String,
    pub length_scale: f32,
    pub speed: f32,
    pub num_threads: i32,
    pub debug: bool,
    pub output_device: Option<String>,
    // VITS-only
    pub noise_scale: f32,
    pub noise_scale_w: f32,
    // Kokoro-only
    pub voices: Option<String>,
    pub dict_dir: Option<String>,
    /// Comma-separated lexicon paths (e.g. `lexicon-us-en.txt,lexicon-zh.txt`).
    /// The sherpa-onnx C API takes a single string with commas as separator.
    pub lexicon: Option<String>,
    pub speaker_id: i32,
    // Matcha-only
    pub vocoder: Option<String>,
    /// Comma-separated rule FST paths (Matcha: `phone.fst,date.fst,number.fst`).
    /// Lives on the top-level `OfflineTtsConfig.rule_fsts` field, not in
    /// `MatchaModelConfig`; the rust binding exposes it on `OfflineTtsConfig`.
    pub rule_fsts: Option<String>,
}

pub struct SherpaTts {
    tts: OfflineTts,
    speed: f32,
    speaker_id: i32,
    output_device: Option<String>,
    backend: String,
}

impl SherpaTts {
    pub fn new(cfg: SherpaTtsConfig) -> Result<Self> {
        let model_config = match cfg.backend.as_str() {
            "vits" => sherpa_onnx::OfflineTtsModelConfig {
                vits: OfflineTtsVitsModelConfig {
                    model: Some(cfg.model.clone()),
                    tokens: Some(cfg.tokens.clone()),
                    noise_scale: cfg.noise_scale,
                    noise_scale_w: cfg.noise_scale_w,
                    length_scale: cfg.length_scale,
                    data_dir: Some(cfg.data_dir.clone()),
                    ..Default::default()
                },
                num_threads: cfg.num_threads,
                debug: cfg.debug,
                ..Default::default()
            },
            "kokoro" => sherpa_onnx::OfflineTtsModelConfig {
                kokoro: OfflineTtsKokoroModelConfig {
                    model: Some(cfg.model.clone()),
                    voices: cfg.voices.clone(),
                    tokens: Some(cfg.tokens.clone()),
                    data_dir: Some(cfg.data_dir.clone()),
                    length_scale: cfg.length_scale,
                    dict_dir: cfg.dict_dir.clone(),
                    lexicon: cfg.lexicon.clone(),
                    ..Default::default()
                },
                num_threads: cfg.num_threads,
                debug: cfg.debug,
                ..Default::default()
            },
            "matcha" => sherpa_onnx::OfflineTtsModelConfig {
                matcha: OfflineTtsMatchaModelConfig {
                    acoustic_model: Some(cfg.model.clone()),
                    vocoder: cfg.vocoder.clone(),
                    lexicon: cfg.lexicon.clone(),
                    tokens: Some(cfg.tokens.clone()),
                    dict_dir: cfg.dict_dir.clone(),
                    length_scale: cfg.length_scale,
                    noise_scale: cfg.noise_scale,
                    ..Default::default()
                },
                num_threads: cfg.num_threads,
                debug: cfg.debug,
                ..Default::default()
            },
            other => {
                anyhow::bail!(
                    "unknown tts.backend {:?} (expected 'vits', 'kokoro', or 'matcha')",
                    other
                );
            }
        };
        let config = OfflineTtsConfig {
            model: model_config,
            rule_fsts: cfg.rule_fsts.clone(),
            ..Default::default()
        };
        let tts = OfflineTts::create(&config)
            .ok_or_else(|| anyhow::anyhow!("failed to create OfflineTts (check model paths)"))?;
        info!(
            "[sherpa tts] loaded backend={} sample_rate={} speakers={} threads={} debug={} \
             length_scale={} speed={} sid={} output_device={:?} rule_fsts={:?}",
            cfg.backend,
            tts.sample_rate(),
            tts.num_speakers(),
            cfg.num_threads,
            cfg.debug,
            cfg.length_scale,
            cfg.speed,
            cfg.speaker_id,
            cfg.output_device,
            cfg.rule_fsts.is_some(),
        );
        Ok(Self {
            tts,
            speed: cfg.speed,
            speaker_id: cfg.speaker_id,
            output_device: cfg.output_device,
            backend: cfg.backend,
        })
    }
}

#[async_trait]
impl TtsSpeaker for SherpaTts {
    async fn speak(
        &self,
        text: &str,
        shutdown: Arc<AtomicBool>,
    ) -> Result<()> {
        if text.is_empty() {
            return Ok(());
        }
        let gen_config = GenerationConfig {
            speed: self.speed,
            sid: self.speaker_id,
            ..Default::default()
        };
        let preview: String = text.chars().take(80).collect();
        let truncated = text.chars().count() > 80;
        info!(
            "[sherpa tts] generating speech backend={} sid={} text={:?}{} ({} chars, speed={})",
            self.backend,
            self.speaker_id,
            preview,
            if truncated { "…" } else { "" },
            text.chars().count(),
            self.speed,
        );
        let started = Instant::now();
        // `GeneratedAudio` holds a `!Send` raw pointer, so we extract
        // everything we need synchronously and let the handle drop
        // before the `await`.
        let (samples, sr, out) = {
            // Callback returns `true` to continue generation, matching
            // the example's `|_, _| true` pattern.
            let audio = self
                .tts
                .generate_with_config(
                    text,
                    &gen_config,
                    Some(|_samples: &[f32], _progress: f32| true),
                )
                .ok_or_else(|| anyhow::anyhow!("TTS generation failed for {:?}", text))?;
            (
                audio.samples().to_vec(),
                audio.sample_rate() as u32,
                self.output_device.clone(),
            )
        };
        let gen_secs = started.elapsed().as_secs_f32();
        let audio_secs = samples.len() as f32 / sr as f32;
        info!(
            "[sherpa tts] generated {} samples ({:.2}s @ {}Hz) in {:.2}s (RTF={:.2})",
            samples.len(),
            audio_secs,
            sr,
            gen_secs,
            gen_secs / audio_secs,
        );
        let play_started = Instant::now();
        let out_label = out.clone();
        tokio::task::spawn_blocking(move || {
            play_samples(&samples, sr, out.as_deref(), &shutdown)
        })
        .await
        .context("tts playback task panicked")??;
        info!(
            "[sherpa tts] playback finished on device={:?} in {:.2}s",
            out_label,
            play_started.elapsed().as_secs_f32()
        );
        Ok(())
    }
}
