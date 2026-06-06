//! Real microphone capture via cpal with VAD-driven endpoint detection.
//!
//! Layout adopted from
//! `/home/xiao/workspace/voice-recognition/src/audio/capture.rs`
//! and `…/src/audio/recorder.rs`:
//!
//!   1. `CpalCapture` (the `AudioCapture` impl) only owns the config;
//!      the actual stream lifecycle lives in a free function.
//!   2. The cpal callback reads in the device's native sample format,
//!      down-mixes to mono f32, and resamples to the target rate
//!      (16 kHz) before pushing to the ringbuf — so the consumer
//!      always works with 16 kHz mono, regardless of the device's
//!      actual capabilities. Down-stream code never has to care
//!      about U16 vs I16 vs F32 or 44.1k vs 48k. The
//!      `convert_audio_for_vad` step in the reference combines the
//!      same three operations in its callback; we do the same.
//!   3. An `Arc<AtomicBool> running` flag is shared between the
//!      cpal callback and the consumer thread — when set to `false`
//!      the callback returns immediately, so dropping the stream
//!      can never leave a callback still trying to push to a
//!      dead ringbuf. This is the safer-shutdown trick from the
//!      reference's `AudioCapture::start` / `stop` split.
//!   4. The stream is opened at the device's *default* sample rate
//!      (we do NOT force 16 kHz). A `LinearResampler` adapts the
//!      device rate down to `cfg.sample_rate` (16 kHz). Reference:
//!      `voice-recognition/src/audio/recorder.rs::convert_audio_for_vad`
//!      and `…/src/audio/format.rs::AudioFormatConverter::resample`.
//!   5. A `last_samples` ring buffer holds the most recent ~30 ms
//!      of audio so the VAD can append the trailing chunk to the
//!      emitted segment, matching the reference's
//!      `RecorderState.last_samples` trick — this avoids chopping
//!      off the last word when speech ends abruptly.
//!
//! Two VAD backends are wired up:
//!   - `cfg.vad_model = Some(_)` → sherpa-onnx Silero VAD, 512-sample
//!     windows at 16 kHz. Far more accurate in noisy rooms.
//!   - `cfg.vad_model = None` → fallback RMS-energy VAD on 30 ms
//!     frames at the target rate. Useful when the silero model
//!     isn't available.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use cpal::traits::{DeviceTrait, StreamTrait};
use cpal::SampleFormat;
use ringbuf::traits::{Consumer, Producer, Split};
use ringbuf::HeapRb;
use tracing::{info, warn};

use crate::traits::AudioCapture;

#[derive(Clone, Debug)]
pub struct CpalCaptureConfig {
    pub device_name: Option<String>,
    /// Target rate that the VAD + ASR consume. Typically 16000.
    /// The cpal callback resamples from the device's native rate
    /// to this value before pushing to the ringbuf.
    pub sample_rate: u32,

    // Silero VAD (used when `vad_model` is `Some`).
    pub vad_model: Option<String>,
    pub vad_threshold: f32,
    pub vad_min_silence_ms: u32,
    pub vad_min_speech_ms: u32,
    pub vad_max_speech_secs: f32,
    pub vad_num_threads: i32,
    pub vad_buffer_secs: f32,

    // Fallback energy VAD (used when `vad_model` is `None`).
    pub rms_threshold: f32,
    pub silence_ms: u32,
    pub pre_speech_ms: u32,
}

pub struct CpalCapture {
    cfg: CpalCaptureConfig,
}

impl CpalCapture {
    pub fn new(cfg: CpalCaptureConfig) -> Self {
        Self { cfg }
    }
}

#[async_trait]
impl AudioCapture for CpalCapture {
    async fn capture_until_silence(
        &self,
        timeout: Duration,
        shutdown: Arc<AtomicBool>,
    ) -> Result<Vec<f32>> {
        let cfg = self.cfg.clone();
        let started = Instant::now();
        info!(
            "[cpal capture] capture_until_silence begin, timeout={:?}, vad={}",
            timeout,
            if cfg.vad_model.is_some() { "silero" } else { "energy" },
        );
        let res = tokio::task::spawn_blocking(move || {
            capture_blocking(cfg, timeout, &shutdown)
        })
        .await
            .context("cpal capture task panicked")?;
        match &res {
            Ok(samples) => info!(
                "[cpal capture] capture_until_silence end, {} samples ({:.2}s) in {:.2}s",
                samples.len(),
                samples.len() as f32 / 16_000.0,
                started.elapsed().as_secs_f32(),
            ),
            Err(e) => warn!(
                "[cpal capture] capture_until_silence failed after {:.2}s: {}",
                started.elapsed().as_secs_f32(),
                e,
            ),
        }
        res
    }
}

/// Linear-interpolation resampler. Mirrors
/// `voice-recognition/src/audio/format.rs::AudioFormatConverter::resample`.
/// Single allocation per resample call; cheap enough for 16 kHz
/// real-time use, and good enough for VAD / ASR feeding.
///
/// `pub(crate)` because the KWS path in `sherpa_kws.rs` needs the
/// same device-rate → 16 kHz downsample that this module does for
/// the capture stream — duplicating the struct there would be
/// worse than the slight visibility leak.
#[derive(Debug)]
pub(crate) struct LinearResampler {
    from_rate: u32,
    to_rate: u32,
    /// Residual fractional index from the previous call, so
    /// back-to-back `resample` calls stay phase-coherent instead of
    /// duplicating samples at the chunk boundary.
    frac: f64,
}

impl LinearResampler {
    pub(crate) fn new(from_rate: u32, to_rate: u32) -> Self {
        Self {
            from_rate,
            to_rate,
            frac: 0.0,
        }
    }

    pub(crate) fn resample(&mut self, input: &[f32]) -> Vec<f32> {
        if self.from_rate == self.to_rate || input.is_empty() {
            return input.to_vec();
        }
        let ratio = self.from_rate as f64 / self.to_rate as f64;
        // +1 for the boundary carry from the previous call.
        let out_cap = (input.len() as f64 / ratio).ceil() as usize + 1;
        let mut out = Vec::with_capacity(out_cap);
        let mut src_idx = self.frac;
        while (src_idx as usize) < input.len() {
            let lo = src_idx as usize;
            let hi = (lo + 1).min(input.len() - 1);
            let frac = src_idx.fract();
            out.push(input[lo] * (1.0 - frac as f32) + input[hi] * frac as f32);
            src_idx += ratio;
        }
        // Carry the unused fractional step into the next chunk so we
        // don't drift on chunk boundaries.
        self.frac = src_idx - input.len() as f64;
        if self.frac >= 1.0 {
            self.frac -= 1.0;
        }
        out
    }
}

fn capture_blocking(
    cfg: CpalCaptureConfig,
    timeout: Duration,
    shutdown: &Arc<AtomicBool>,
) -> Result<Vec<f32>> {
    // 1. Open device via the shared `audio_devices` helper (handles
    //    None / exact / substring matching + cpal default fallback).
    let device = crate::audio_devices::find_input_device(cfg.device_name.as_deref())?;
    let device_label = device.name().unwrap_or_else(|_| "?".into());
    info!("[cpal capture] using input device: {}", device_label);

    // 2. Use the device's default config (no rate forcing). The
    //    reference project does the same — letting cpal pick the
    //    device's native rate avoids `unsupported sample rate`
    //    errors on devices that only do 48 kHz.
    let stream_cfg = device.default_input_config()?;
    let actual_rate = stream_cfg.sample_rate().0;
    let channels = stream_cfg.channels() as usize;
    let sample_format = stream_cfg.sample_format();
    info!(
        "[cpal capture] device format={:?} channels={} rate={} (target={}Hz, callback will resample)",
        sample_format, channels, actual_rate, cfg.sample_rate,
    );

    // 3. Ringbuf holds 30 s of mono f32 at the *target* rate (the
    //    resampler converts inside the callback). One ringbuf is
    //    shared across all three format callbacks.
    let buf_len = (cfg.sample_rate as usize) * 30;
    let rb = HeapRb::<f32>::new(buf_len);
    let (mut prod, mut cons) = rb.split();

    // 4. Shared stop flag + resampler state. The resampler lives
    //    behind a Mutex because it carries the phase-coherent
    //    fractional-index state across callback invocations; in
    //    practice the lock is uncontended because only the cpal
    //    callback thread ever touches it.
    //
    //    The `running` flag used to be a local `Arc<AtomicBool>`
    //    that was only ever flipped to `false` *after* capture
    //    returned. The consumer loops' `if !running.load()` check
    //    was therefore dead code, which meant Ctrl+C during
    //    recording would block for up to the capture timeout
    //    (typically 30 s) before the cpal stream was dropped. We
    //    now use the orchestrator's process-wide shutdown flag
    //    directly: when the Ctrl+C handler flips it, the consumer
    //    loop bails on its next iteration, the cpal callback goes
    //    no-op, and dropping the `stream` releases the device.
    let resampler = Arc::new(Mutex::new(LinearResampler::new(
        actual_rate,
        cfg.sample_rate,
    )));

    // 5. Build the cpal stream. The callback is `FnMut + Send + 'static`
    //    (it captures `prod` + `running_for_cb` + `resampler_for_cb`),
    //    so the format dispatch mirrors the reference's three-arm match.
    let running_for_cb = shutdown.clone();
    let resampler_for_cb = resampler.clone();
    let stream = match sample_format {
        SampleFormat::F32 => device.build_input_stream(
            &stream_cfg.into(),
            move |data: &[f32], _| {
                if running_for_cb.load(Ordering::SeqCst) {
                    return;
                }
                let mono = downmix_f32(data, channels);
                let resampled = resampler_for_cb
                    .lock()
                    .map(|mut r| r.resample(&mono))
                    .unwrap_or_default();
                push_all(&mut prod, &resampled);
            },
            err_fn,
            None,
        )?,
        SampleFormat::I16 => device.build_input_stream(
            &stream_cfg.into(),
            move |data: &[i16], _| {
                if running_for_cb.load(Ordering::SeqCst) {
                    return;
                }
                let mono = downmix_i16(data, channels);
                let resampled = resampler_for_cb
                    .lock()
                    .map(|mut r| r.resample(&mono))
                    .unwrap_or_default();
                push_all(&mut prod, &resampled);
            },
            err_fn,
            None,
        )?,
        SampleFormat::U16 => device.build_input_stream(
            &stream_cfg.into(),
            move |data: &[u16], _| {
                if running_for_cb.load(Ordering::SeqCst) {
                    return;
                }
                let mono = downmix_u16(data, channels);
                let resampled = resampler_for_cb
                    .lock()
                    .map(|mut r| r.resample(&mono))
                    .unwrap_or_default();
                push_all(&mut prod, &resampled);
            },
            err_fn,
            None,
        )?,
        other => bail!("unsupported input sample format: {:?}", other),
    };
    stream.play()?;

    // 6. Run the VAD consumer loop. The two paths are split because
    //    silero is the one that actually needs 16 kHz resampled
    //    input; the energy VAD is also happy to consume at 16 kHz
    //    (the rate-agnostic threshold in `rms_threshold` works the
    //    same at any rate).
    let result = if cfg.vad_model.is_some() {
        capture_with_silero(cfg, &mut cons, timeout, shutdown)
    } else {
        capture_with_energy(cfg, &mut cons, timeout, shutdown)
    };

    // 7. Clean shutdown: drop the stream (cpal stops the audio
    //    thread). The shutdown flag is *not* flipped here — it
    //    stays set so a subsequent capture that somehow runs (it
    //    shouldn't, but defensively) sees the flag and bails
    //    immediately.
    drop(stream);
    result
}

fn capture_with_silero(
    cfg: CpalCaptureConfig,
    cons: &mut impl Consumer<Item = f32>,
    timeout: Duration,
    running: &Arc<AtomicBool>,
) -> Result<Vec<f32>> {
    let model_path = cfg
        .vad_model
        .as_deref()
        .expect("silero path requires vad_model");
    let mut silero = sherpa_onnx::SileroVadModelConfig::default();
    silero.model = Some(model_path.to_string());
    silero.threshold = cfg.vad_threshold;
    silero.min_silence_duration = cfg.vad_min_silence_ms as f32 / 1000.0;
    silero.min_speech_duration = cfg.vad_min_speech_ms as f32 / 1000.0;
    silero.max_speech_duration = cfg.vad_max_speech_secs;
    silero.window_size = 512;

    let vad_config = sherpa_onnx::VadModelConfig {
        silero_vad: silero,
        ten_vad: Default::default(),
        sample_rate: cfg.sample_rate as i32,
        num_threads: cfg.vad_num_threads,
        provider: Some("cpu".to_string()),
        debug: false,
    };
    let vad = sherpa_onnx::VoiceActivityDetector::create(&vad_config, cfg.vad_buffer_secs)
        .ok_or_else(|| anyhow::anyhow!("failed to create silero VAD (check vad_model path)"))?;
    info!(
        "[cpal capture] silero VAD: threshold={} min_silence={}ms min_speech={}ms max_speech={}s",
        cfg.vad_threshold,
        cfg.vad_min_silence_ms,
        cfg.vad_min_speech_ms,
        cfg.vad_max_speech_secs,
    );

    const WINDOW: usize = 512;
    let mut window = [0.0f32; WINDOW];
    // Ring buffer of the most recent ~30 ms at the target rate
    // (16 kHz). At `SpeechComplete` we append this to the VAD's
    // own segment so the user doesn't lose the last word.
    // Matches the reference's `RecorderState.last_samples` trick.
    let last_keep = (cfg.sample_rate as usize) * 30 / 1000;
    let mut last_samples: Vec<f32> = Vec::with_capacity(last_keep.max(1));
    let deadline = Instant::now() + timeout;

    loop {
        // `running` is the global shutdown flag (default false,
        // flipped true by the Ctrl+C handler). The check is
        // therefore `running.load()` — bail when set. Earlier
        // revisions used a local `running: Arc<AtomicBool>::new(true)`
        // and the inverse check (`!running.load()`), which worked
        // when the local flag was set to false on stop. When the
        // refactor switched to the global shutdown flag it missed
        // the inversion: the predicate then bailed on the FIRST
        // iteration when shutdown was unset (i.e. always), which
        // is exactly the "after wake, capture returns 0 samples
        // in 60 ms" regression this fix is for.
        if running.load(Ordering::SeqCst) {
            return Ok(Vec::new());
        }
        if Instant::now() >= deadline {
            // Timeout: flush and return whatever segment the VAD
            // has accumulated. If nothing is ready, return empty
            // (orchestrator treats that as a timeout).
            return finish_with_flush(&vad, &last_samples, cfg.sample_rate, /*from_timeout=*/ true);
        }
        let got = cons.pop_slice(&mut window);
        if got == 0 {
            std::thread::sleep(Duration::from_millis(5));
            continue;
        }
        if got < WINDOW {
            // VAD's internal ring expects fixed-size 512-sample
            // windows. Zero-pad the trailing partial window so the
            // VAD can advance its buffer.
            for s in &mut window[got..] {
                *s = 0.0;
            }
        }
        // Update `last_samples` BEFORE the VAD consumes the
        // window — that's the trailing audio the user just said.
        for &s in &window {
            if last_samples.len() == last_keep {
                last_samples.remove(0);
            }
            last_samples.push(s);
        }
        vad.accept_waveform(&window);
        if let Some(seg) = vad.front() {
            let mut samples = seg.samples().to_vec();
            vad.pop();
            let trailing = last_samples.len().min(samples.len() / 2);
            samples.extend_from_slice(&last_samples[..trailing]);
            info!(
                "[cpal capture] silero VAD emitted segment: {} samples ({:.2}s) + {} trailing samples",
                samples.len(),
                samples.len() as f32 / cfg.sample_rate as f32,
                trailing,
            );
            return Ok(samples);
        }
    }
}

fn finish_with_flush(
    vad: &sherpa_onnx::VoiceActivityDetector,
    last_samples: &[f32],
    sample_rate: u32,
    from_timeout: bool,
) -> Result<Vec<f32>> {
    vad.flush();
    let Some(seg) = vad.front() else {
        return Ok(Vec::new());
    };
    let mut samples = seg.samples().to_vec();
    vad.pop();
    let trailing = last_samples.len().min(samples.len() / 2);
    samples.extend_from_slice(&last_samples[..trailing]);
    if from_timeout {
        info!(
            "[cpal capture] silero VAD timeout flush: {} samples ({:.2}s) after trailing {} samples",
            samples.len(),
            samples.len() as f32 / sample_rate as f32,
            trailing,
        );
    }
    Ok(samples)
}

fn capture_with_energy(
    cfg: CpalCaptureConfig,
    cons: &mut impl Consumer<Item = f32>,
    timeout: Duration,
    running: &Arc<AtomicBool>,
) -> Result<Vec<f32>> {
    warn!("[cpal capture] using fallback RMS energy VAD (consider setting voice.vad_model)");
    const WINDOW_MS: u32 = 30;
    let actual_rate = cfg.sample_rate;
    let window = ((actual_rate as usize) * WINDOW_MS as usize) / 1000;
    let silence_windows = (cfg.silence_ms / WINDOW_MS) as usize;
    let pre_speech_samples = ((actual_rate as usize) * cfg.pre_speech_ms as usize) / 1000;

    let mut collected: Vec<f32> = Vec::new();
    let mut pre_speech: Vec<f32> = Vec::with_capacity(pre_speech_samples.max(1));
    let mut recent: Vec<f32> = Vec::with_capacity(window.max(1));
    let mut speech_started = false;
    let mut silence_count = 0usize;
    let deadline = Instant::now() + timeout;

    while Instant::now() < deadline {
        if running.load(Ordering::SeqCst) {
            return Ok(Vec::new());
        }
        let mut chunk = vec![0.0f32; 1024];
        let got = cons.pop_slice(&mut chunk);
        if got == 0 {
            std::thread::sleep(Duration::from_millis(5));
            continue;
        }
        chunk.truncate(got);

        for &s in &chunk {
            if speech_started {
                collected.push(s);
            } else {
                if pre_speech.len() == pre_speech_samples {
                    pre_speech.remove(0);
                }
                pre_speech.push(s);
            }
            if recent.len() == window {
                recent.remove(0);
            }
            recent.push(s);
            if recent.len() == window {
                let rms = rms(&recent);
                if rms > cfg.rms_threshold {
                    if !speech_started {
                        speech_started = true;
                        collected.extend(pre_speech.drain(..));
                    }
                    silence_count = 0;
                } else if speech_started {
                    silence_count += 1;
                    if silence_count >= silence_windows {
                        return Ok(collected);
                    }
                }
            }
        }
    }

    if speech_started {
        Ok(collected)
    } else {
        Ok(Vec::new())
    }
}

fn rms(samples: &[f32]) -> f32 {
    let sum: f32 = samples.iter().map(|s| s * s).sum();
    (sum / samples.len().max(1) as f32).sqrt()
}

// ── format-specific downmixers ─────────────────────────────────────
// Each one reads interleaved multi-channel samples and returns a
// mono f32 buffer in [-1, 1] (or whatever the source format's
// natural range is — the ringbuf only cares about f32).
//
// `pub(crate)` because the KWS path in `sherpa_kws.rs` shares the
// same callback format-dispatch and would otherwise have to
// re-implement (and unit-test) the same three downmix functions.

pub(crate) fn downmix_f32(data: &[f32], channels: usize) -> Vec<f32> {
    if channels == 1 {
        return data.to_vec();
    }
    data.chunks(channels)
        .map(|frame| frame.iter().copied().sum::<f32>() / channels as f32)
        .collect()
}

pub(crate) fn downmix_i16(data: &[i16], channels: usize) -> Vec<f32> {
    data.chunks(channels)
        .map(|frame| {
            let sum: f32 = frame.iter().map(|&s| s as f32 / i16::MAX as f32).sum();
            sum / channels as f32
        })
        .collect()
}

pub(crate) fn downmix_u16(data: &[u16], channels: usize) -> Vec<f32> {
    data.chunks(channels)
        .map(|frame| {
            let sum: f32 = frame
                .iter()
                .map(|&s| ((s as f32) - 32768.0) / 32768.0)
                .sum();
            sum / channels as f32
        })
        .collect()
}

pub(crate) fn push_all(prod: &mut impl Producer<Item = f32>, samples: &[f32]) {
    for &s in samples {
        let _ = prod.try_push(s);
    }
}

fn err_fn(err: cpal::StreamError) {
    warn!("[cpal capture] stream error: {:?}", err);
}

#[cfg(test)]
mod tests {
    use super::{downmix_f32, downmix_i16, downmix_u16, CpalCaptureConfig, LinearResampler};
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    #[test]
    fn resampler_passthrough_when_rates_match() {
        let mut r = LinearResampler::new(16000, 16000);
        let input: Vec<f32> = (0..32).map(|i| i as f32).collect();
        let out = r.resample(&input);
        assert_eq!(out, input);
    }

    #[test]
    fn resampler_drops_when_target_is_lower() {
        // 48 kHz → 16 kHz should produce ~1/3 the input length.
        let mut r = LinearResampler::new(48_000, 16_000);
        let input: Vec<f32> = (0..480).map(|i| i as f32).collect();
        let out = r.resample(&input);
        assert!(
            (140..=160).contains(&out.len()),
            "expected ~160 samples, got {}",
            out.len()
        );
    }

    #[test]
    fn resampler_interpolates_linearly() {
        // 4 kHz → 8 kHz: a 4-sample input gives ~8 output samples
        // via linear interpolation. The reference's behavior is
        // "hold last sample" at the input boundary, which means
        // the last position in the output is the same as the last
        // input sample (1.0 here), not an interpolated value.
        let mut r = LinearResampler::new(4_000, 8_000);
        let input = vec![0.0, 1.0, 0.0, 1.0];
        let out = r.resample(&input);
        // Boundary carry can drop the very last sample, so the
        // length is 7 or 8.
        assert!(out.len() >= 7 && out.len() <= 8, "len = {}", out.len());
        // out[0] is at src_idx=0, the first input sample.
        assert!((out[0] - 0.0).abs() < 1e-3, "out[0] = {}", out[0]);
        // out[1] is at src_idx=0.5, between input[0]=0 and
        // input[1]=1 → 0.5.
        assert!(
            (out[1] - 0.5).abs() < 1e-3,
            "expected out[1] ~0.5, got {}",
            out[1]
        );
        // out[2] is at src_idx=1.0, exactly input[1]=1.
        assert!((out[2] - 1.0).abs() < 1e-3, "out[2] = {}", out[2]);
        // out[3] is at src_idx=1.5, between input[1]=1 and
        // input[2]=0 → 0.5.
        assert!(
            (out[3] - 0.5).abs() < 1e-3,
            "expected out[3] ~0.5, got {}",
            out[3]
        );
    }

    #[test]
    fn resampler_is_phase_coherent_across_chunks() {
        // Feeding 8 input samples in one chunk vs two chunks of 4
        // should produce the same *interior* output. The very last
        // sample of a chunk "holds" the previous value (matches
        // the reference's `if src_idx_floor < samples.len()`
        // branch), so a 1-sample drift at the boundary is
        // expected and acceptable for streaming use — the VAD/ASR
        // downstream don't care.
        let input: Vec<f32> = vec![0.0, 1.0, 0.0, 1.0, 0.0, 1.0, 0.0, 1.0];
        let mut r1 = LinearResampler::new(4_000, 8_000);
        let one_shot = r1.resample(&input);

        let mut r2 = LinearResampler::new(4_000, 8_000);
        let a = r2.resample(&input[..4]);
        let _b = r2.resample(&input[4..]);
        // Phase coherence on the interior: drop the last sample
        // of the first chunk (boundary) from the comparison.
        let interior = a.len().saturating_sub(1);
        for i in 0..interior {
            assert!(
                (one_shot[i] - a[i]).abs() < 1e-3,
                "diverged at i={}: one_shot={} chunked={}",
                i,
                one_shot[i],
                a[i],
            );
        }
    }

    #[test]
    fn downmix_f32_mono_passthrough() {
        let data = vec![0.1, 0.2, 0.3];
        let out = downmix_f32(&data, 1);
        assert_eq!(out, data);
    }

    #[test]
    fn downmix_f32_stereo_averages() {
        // L=1.0, R=0.0 → mono=0.5; L=0.4, R=0.6 → mono=0.5.
        let data = vec![1.0, 0.0, 0.4, 0.6];
        let out = downmix_f32(&data, 2);
        assert_eq!(out, vec![0.5, 0.5]);
    }

    #[test]
    fn downmix_i16_normalizes_to_f32() {
        // i16::MAX / i16::MAX as f32 in a stereo frame → 1.0.
        let data = vec![i16::MAX, i16::MAX];
        let out = downmix_i16(&data, 2);
        assert!((out[0] - 1.0).abs() < 1e-3, "out = {}", out[0]);
    }

    #[test]
    fn downmix_u16_centers_around_zero() {
        // u16 32768 (mid-scale) → 0.0 after centering.
        let data = vec![32768u16];
        let out = downmix_u16(&data, 1);
        assert!((out[0] - 0.0).abs() < 1e-3, "out = {}", out[0]);
    }

    /// Regression for the "capture returns 0 samples in 60 ms after
    /// wake" bug. The `running` parameter is the global shutdown
    /// flag (default `false`, set to `true` on Ctrl+C). The check
    /// inside the VAD loop is therefore `running.load()` — bail
    /// when set — NOT `!running.load()`. The old code used a
    /// local `running: Arc<AtomicBool>::new(true)` and the inverse
    /// predicate; the refactor that swapped the local flag for
    /// the global shutdown missed the inversion, so the loop
    /// bailed on the FIRST iteration every time (when the flag
    /// was unset, which is the default).
    ///
    /// This test uses the energy VAD path (no silero model
    /// required) and an always-empty consumer. With shutdown
    /// unset, the loop must spin until the deadline, not bail
    /// after one iteration. We use a 200 ms timeout and assert
    /// the call takes at least 100 ms (i.e. it really waited
    /// instead of returning in <5 ms on iter 1).
    #[test]
    fn energy_vad_loop_respects_unset_shutdown() {
        use ringbuf::traits::Split;
        use ringbuf::HeapRb;

        let cfg = CpalCaptureConfig {
            device_name: None,
            sample_rate: 16000,
            vad_model: None,
            vad_threshold: 0.5,
            vad_min_silence_ms: 500,
            vad_min_speech_ms: 250,
            vad_max_speech_secs: 20.0,
            vad_num_threads: 1,
            vad_buffer_secs: 30.0,
            rms_threshold: 0.01,
            silence_ms: 500,
            pre_speech_ms: 300,
        };
        // Always-empty consumer — the loop has nothing to read,
        // so the only way to return is via the deadline or the
        // shutdown check.
        let rb = HeapRb::<f32>::new(1024);
        let (_prod, mut cons) = rb.split();
        // Shutdown is NOT set — the default state.
        let shutdown = Arc::new(AtomicBool::new(false));

        let started = Instant::now();
        let res = super::capture_with_energy(
            cfg,
            &mut cons,
            Duration::from_millis(200),
            &shutdown,
        )
        .expect("capture_with_energy should not error");
        let elapsed = started.elapsed();

        // It should have waited the full timeout (no audio came
        // in, no speech detected, no shutdown). The pre-fix bug
        // returned in well under 5 ms.
        assert!(
            elapsed >= Duration::from_millis(100),
            "energy VAD loop returned in {elapsed:?} — the shutdown check is probably inverted (would have bailed on iter 1)"
        );
        // The result is empty because no speech was detected.
        assert!(res.is_empty(), "expected empty result, got {} samples", res.len());
    }

    /// And the inverse: when shutdown IS set, the loop must bail
    /// promptly. This is the path Ctrl+C relies on to release
    /// the audio device without waiting out the full capture
    /// timeout (8 s in production).
    #[test]
    fn energy_vad_loop_bails_when_shutdown_set() {
        use ringbuf::traits::Split;
        use ringbuf::HeapRb;

        let cfg = CpalCaptureConfig {
            device_name: None,
            sample_rate: 16000,
            vad_model: None,
            vad_threshold: 0.5,
            vad_min_silence_ms: 500,
            vad_min_speech_ms: 250,
            vad_max_speech_secs: 20.0,
            vad_num_threads: 1,
            vad_buffer_secs: 30.0,
            rms_threshold: 0.01,
            silence_ms: 500,
            pre_speech_ms: 300,
        };
        let rb = HeapRb::<f32>::new(1024);
        let (_prod, mut cons) = rb.split();
        let shutdown = Arc::new(AtomicBool::new(true));

        let started = Instant::now();
        let res = super::capture_with_energy(
            cfg,
            &mut cons,
            Duration::from_secs(8), // would otherwise take 8 s
            &shutdown,
        )
        .expect("capture_with_energy should not error");
        let elapsed = started.elapsed();

        assert!(
            elapsed < Duration::from_millis(500),
            "loop should bail quickly on shutdown, took {elapsed:?}"
        );
        assert!(res.is_empty(), "expected empty result on shutdown");
    }
}
