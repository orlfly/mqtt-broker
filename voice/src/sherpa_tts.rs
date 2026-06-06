//! Offline VITS TTS (sherpa-onnx) wrapped to implement `TtsSpeaker`.
//!
//! Mirrors the layout of
//! `sherpa-onnx/rust-api-examples/examples/vits_tts.rs`:
//!
//!   1. Build an `OfflineTtsConfig { model: OfflineTtsModelConfig { vits: ... } }`.
//!   2. Call `tts.generate_with_config(text, &gen_config, Some(callback))` —
//!      the callback receives `(samples, progress)` chunks and returns
//!      `true` to keep generating.
//!   3. Move the produced samples off the `GeneratedAudio` handle (which
//!      holds a `!Send` raw pointer) before `await`ing the playback task.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use async_trait::async_trait;
use sherpa_onnx::{
    GenerationConfig, OfflineTts, OfflineTtsConfig, OfflineTtsVitsModelConfig,
};
use tracing::info;

use crate::cpal_playback::play_samples;
use crate::traits::TtsSpeaker;

#[derive(Clone, Debug)]
pub struct SherpaTtsConfig {
    pub model: String,
    pub tokens: String,
    pub data_dir: String,
    pub length_scale: f32,
    pub speed: f32,
    pub noise_scale: f32,
    pub noise_scale_w: f32,
    pub num_threads: i32,
    pub debug: bool,
    pub output_device: Option<String>,
}

pub struct SherpaTts {
    tts: OfflineTts,
    speed: f32,
    output_device: Option<String>,
}

impl SherpaTts {
    pub fn new(cfg: SherpaTtsConfig) -> Result<Self> {
        // Build the config in the same shape as
        // rust-api-examples/examples/vits_tts.rs (Piper VITS).
        let config = OfflineTtsConfig {
            model: sherpa_onnx::OfflineTtsModelConfig {
                vits: OfflineTtsVitsModelConfig {
                    model: Some(cfg.model),
                    tokens: Some(cfg.tokens),
                    noise_scale: cfg.noise_scale,
                    noise_scale_w: cfg.noise_scale_w,
                    length_scale: cfg.length_scale,
                    data_dir: Some(cfg.data_dir),
                    ..Default::default()
                },
                num_threads: cfg.num_threads,
                debug: cfg.debug,
                ..Default::default()
            },
            ..Default::default()
        };
        let tts = OfflineTts::create(&config)
            .ok_or_else(|| anyhow::anyhow!("failed to create OfflineTts (check model paths)"))?;
        info!(
            "[sherpa tts] loaded OfflineTts sample_rate={} speakers={} threads={} debug={} \
             noise_scale={} noise_scale_w={} length_scale={} output_device={:?}",
            tts.sample_rate(),
            tts.num_speakers(),
            cfg.num_threads,
            cfg.debug,
            cfg.noise_scale,
            cfg.noise_scale_w,
            cfg.length_scale,
            cfg.output_device,
        );
        Ok(Self {
            tts,
            speed: cfg.speed,
            output_device: cfg.output_device,
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
            ..Default::default()
        };
        let preview: String = text.chars().take(80).collect();
        let truncated = text.chars().count() > 80;
        info!(
            "[sherpa tts] generating speech for text={:?}{} ({} chars, speed={})",
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
