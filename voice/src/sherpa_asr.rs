//! Streaming Zipformer (sherpa-onnx) ASR wrapped to implement
//! `AsrTranscriber`.
//!
//! Mirrors the structure of
//! `sherpa-onnx/rust-api-examples/examples/streaming_zipformer.rs`:
//!
//!   1. Build `OnlineRecognizerConfig` with `enable_endpoint = true`.
//!   2. For each chunk of input audio:
//!        `stream.accept_waveform(sr, chunk)` → drain
//!        `while recognizer.is_ready { decode; get_result; if is_endpoint { reset } }`.
//!   3. After the last chunk, feed ~0.3 s of trailing silence, call
//!      `stream.input_finished()`, and drain the recognizer one more
//!      time so the final text comes out.
//!
//! The example works on a `Wave`; we accept raw `&[f32]` because the
//! agent's `cpal_capture` already returns mono f32 from the Silero VAD
//! endpoint.

use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use async_trait::async_trait;
use sherpa_onnx::{OnlineRecognizer, OnlineRecognizerConfig};
use tracing::info;

use crate::traits::AsrTranscriber;

#[derive(Clone, Debug)]
pub struct SherpaAsrConfig {
    pub encoder: String,
    pub decoder: String,
    pub joiner: String,
    pub tokens: String,
    pub num_threads: i32,
    pub provider: String,
    pub debug: bool,
    pub sample_rate: i32,
}

pub struct SherpaStreamingAsr {
    recognizer: Arc<OnlineRecognizer>,
    sample_rate: i32,
}

impl SherpaStreamingAsr {
    pub fn new(cfg: SherpaAsrConfig) -> Result<Self> {
        // Build the config in the same shape as
        // rust-api-examples/examples/streaming_zipformer.rs.
        let mut config = OnlineRecognizerConfig::default();
        config.model_config.transducer.encoder = Some(cfg.encoder.clone());
        config.model_config.transducer.decoder = Some(cfg.decoder.clone());
        config.model_config.transducer.joiner = Some(cfg.joiner.clone());
        config.model_config.tokens = Some(cfg.tokens.clone());
        config.model_config.provider = Some(cfg.provider.clone());
        config.model_config.num_threads = cfg.num_threads;
        config.model_config.debug = cfg.debug;
        config.enable_endpoint = true;
        config.decoding_method = Some("greedy_search".to_string());

        let recognizer = OnlineRecognizer::create(&config).ok_or_else(|| {
            anyhow::anyhow!("failed to create OnlineRecognizer (check model paths)")
        })?;
        info!(
            "[sherpa asr] loaded OnlineRecognizer provider={} threads={} debug={} endpoint=true",
            cfg.provider, cfg.num_threads, cfg.debug
        );
        Ok(Self {
            recognizer: Arc::new(recognizer),
            sample_rate: cfg.sample_rate,
        })
    }

    pub fn recognizer(&self) -> Arc<OnlineRecognizer> {
        self.recognizer.clone()
    }

    pub fn sample_rate(&self) -> i32 {
        self.sample_rate
    }
}

#[async_trait]
impl AsrTranscriber for SherpaStreamingAsr {
    async fn transcribe(&self, samples: &[f32]) -> Result<String> {
        if samples.is_empty() {
            return Ok(String::new());
        }
        let recognizer = self.recognizer.clone();
        let sample_rate = self.sample_rate;
        let samples = samples.to_vec();
        let audio_secs = samples.len() as f32 / sample_rate as f32;
        info!(
            "[sherpa asr] transcribing {} samples ({:.2}s @ {}Hz)",
            samples.len(),
            audio_secs,
            sample_rate
        );
        let text = tokio::task::spawn_blocking(move || -> Result<String> {
            let started = Instant::now();
            // One stream per call (the example's `recognizer.create_stream()`).
            let stream = recognizer.create_stream();
            // Any positive value works; matches the example's CHUNK_SIZE.
            const CHUNK_SIZE: usize = 3200;

            for chunk in samples.chunks(CHUNK_SIZE) {
                stream.accept_waveform(sample_rate, chunk);
                while recognizer.is_ready(&stream) {
                    recognizer.decode(&stream);
                    if recognizer.is_endpoint(&stream) {
                        // Drop this utterance so the next chunk starts
                        // a fresh segment — same as the example's
                        // `is_endpoint → reset` branch.
                        recognizer.reset(&stream);
                    }
                }
            }

            // Tail padding (~0.3 s of silence) so the decoder can flush
            // whatever it has buffered.
            let tail_padding_len = (sample_rate as f32 * 0.3).round() as usize;
            let tail_padding = vec![0.0f32; tail_padding_len];
            stream.accept_waveform(sample_rate, &tail_padding);
            stream.input_finished();

            // Final drain — picks up the trailing tokens that the
            // endpoint / padding flush released.
            let mut final_text = String::new();
            while recognizer.is_ready(&stream) {
                recognizer.decode(&stream);
                if let Some(result) = recognizer.get_result(&stream) {
                    if !result.text.is_empty() {
                        final_text = result.text;
                    }
                }
            }
            let elapsed = started.elapsed();
            info!(
                "[sherpa asr] transcribed {:?} ({} chars) in {:.2}s (RTF={:.2})",
                final_text,
                final_text.chars().count(),
                elapsed.as_secs_f32(),
                elapsed.as_secs_f32() / audio_secs,
            );
            Ok(final_text)
        })
        .await
        .context("sherpa asr task panicked")??;
        Ok(text)
    }
}
