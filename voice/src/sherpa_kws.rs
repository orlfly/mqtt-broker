//! Wake detector backed by sherpa-onnx's `KeywordSpotter`.
//!
//! Mirrors the streaming-microphone layout from
//! `sherpa-onnx/rust-api-examples/examples/keyword_spotter.rs`:
//!
//!   1. Build a `KeywordSpotterConfig` with the model paths + tokens.
//!   2. Open the cpal input stream, push mono f32 into a ring buffer.
//!   3. In the consumer loop, feed the spotter one chunk at a time:
//!        `stream.accept_waveform(model_rate, &chunk)`
//!      then drain the spotter with
//!        `while kws.is_ready(&stream) { kws.decode(&stream); ... }`.
//!   4. On a hit, call `kws.reset(&stream)` (the example resets before
//!      the next detection) and return the alias.
//!
//! Inline extra keywords are passed via
//! `kws.create_stream_with_keywords("<tokens> @<alias>/...")` instead
//! of writing a keywords file.

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

use crate::cpal_capture::{downmix_f32, downmix_i16, downmix_u16, push_all, LinearResampler};
use crate::traits::{WakeDetector, WakeEvent};

#[derive(Clone, Debug)]
pub struct SherpaKwsConfig {
    pub encoder: String,
    pub decoder: String,
    pub joiner: String,
    pub tokens: String,
    /// Optional path to a keywords file shipped with the model or authored
    /// by the user. Format: one keyword per line, e.g.
    ///     n ǐ h ǎo x iǎo zh ù @你好小助
    pub keywords_file: Option<String>,
    /// Additional keywords added on top of `keywords_file`. Each entry is a
    /// pinyin-token string with an `@alias` suffix. Multiple keywords are
    /// joined with `/` per sherpa-onnx convention.
    pub keywords_inline: Vec<String>,
    pub num_threads: i32,
    pub provider: String,
    pub debug: bool,
    pub sample_rate: i32,
    pub device_name: Option<String>,
}

pub struct SherpaKws {
    inner: Arc<SherpaKwsInner>,
}

struct SherpaKwsInner {
    kws: sherpa_onnx::KeywordSpotter,
    /// Inline keyword strings in the format `<pinyin tokens> @<alias>`.
    /// Used to look up the human-readable alias when a hit lands on one
    /// of these entries.
    keywords_inline: Vec<InlineKeyword>,
    sample_rate: i32,
    device_name: Option<String>,
}

#[derive(Clone, Debug)]
struct InlineKeyword {
    tokens: String,
    alias: String,
}

impl SherpaKws {
    pub fn new(cfg: SherpaKwsConfig) -> Result<Self> {
        let keywords_inline: Vec<InlineKeyword> = cfg
            .keywords_inline
            .iter()
            .map(|s| parse_inline_keyword(s))
            .collect();

        // Resolve the keywords source in this order (KeywordSpotter rejects
        // configs where both `keywords_file` and `keywords_buf` are empty):
        //   1. explicit `kws_keywords_file` from broker.yaml
        //   2. bundled `<kws_encoder dir>/keywords.txt` (the WenetSpeech
        //      3.3M model ships one)
        //   3. joined inline entries (used as `keywords_buf` so validation
        //      passes when neither path exists)
        let keywords_file = match cfg.keywords_file.clone() {
            Some(p) => Some(p),
            None => std::path::Path::new(&cfg.encoder)
                .parent()
                .map(|p| p.join("keywords.txt"))
                .filter(|p| p.is_file())
                .map(|p| p.to_string_lossy().to_string()),
        };
        let keywords_buf = if keywords_file.is_none() && !keywords_inline.is_empty() {
            Some(
                keywords_inline
                    .iter()
                    .map(|k| k.tokens.clone())
                    .collect::<Vec<_>>()
                    .join("/"),
            )
        } else {
            None
        };

        // Build the config in the same shape as
        // rust-api-examples/examples/keyword_spotter.rs.
        let mut config = sherpa_onnx::KeywordSpotterConfig::default();
        config.model_config.transducer.encoder = Some(cfg.encoder.clone());
        config.model_config.transducer.decoder = Some(cfg.decoder.clone());
        config.model_config.transducer.joiner = Some(cfg.joiner.clone());
        config.model_config.tokens = Some(cfg.tokens.clone());
        config.model_config.provider = Some(cfg.provider.clone());
        config.model_config.num_threads = cfg.num_threads;
        config.model_config.debug = cfg.debug;
        config.keywords_file = keywords_file.clone();
        config.keywords_buf = keywords_buf;

        let kws = sherpa_onnx::KeywordSpotter::create(&config).ok_or_else(|| {
            anyhow::anyhow!(
                "failed to create KeywordSpotter. Provide at least one of \
                 `voice.kws_keywords_file` (path to a keywords.txt), or \
                 `voice.kws_keywords_inline` (pinyin-token entries)."
            )
        })?;
        info!(
            "[sherpa kws] loaded KeywordSpotter provider={} threads={} debug={} \
             keywords_file={:?} inline_entries={}",
            cfg.provider,
            cfg.num_threads,
            cfg.debug,
            keywords_file,
            keywords_inline.len()
        );
        Ok(Self {
            inner: Arc::new(SherpaKwsInner {
                kws,
                keywords_inline,
                sample_rate: cfg.sample_rate,
                device_name: cfg.device_name,
            }),
        })
    }

    /// Build a `SherpaKwsConfig` from the agent's `VoiceChannelConfig`.
    /// This is the single source of truth for the KWS field mapping
    /// — `voice::build_stack` and the `VOICE_DUMP_WAKE_TEST` diagnostic
    /// path in `agent/src/main.rs` both go through it, so the live
    /// KWS and the wake-test never drift.
    pub fn config_from_voice_channel(
        cfg: &common::VoiceChannelConfig,
    ) -> Result<SherpaKwsConfig> {
        fn required(opt: &Option<String>, name: &str) -> Result<String> {
            opt.as_ref()
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("voice.{} is required for engine='sherpa'", name))
        }
        Ok(SherpaKwsConfig {
            encoder: required(&cfg.kws_encoder, "kws_encoder")?,
            decoder: required(&cfg.kws_decoder, "kws_decoder")?,
            joiner: required(&cfg.kws_joiner, "kws_joiner")?,
            tokens: required(&cfg.kws_tokens, "kws_tokens")?,
            keywords_file: cfg.kws_keywords_file.clone(),
            keywords_inline: cfg.kws_keywords_inline.clone(),
            num_threads: cfg.kws_num_threads,
            provider: cfg.kws_provider.clone(),
            debug: cfg.kws_debug,
            sample_rate: cfg.sample_rate as i32,
            device_name: cfg.audio_input_device.clone(),
        })
    }
}

/// Parse `<pinyin tokens> @<alias>` into (tokens, alias). If no `@` is
/// present the alias defaults to the token string itself.
fn parse_inline_keyword(s: &str) -> InlineKeyword {
    match s.rsplit_once('@') {
        Some((tokens, alias)) => InlineKeyword {
            tokens: tokens.trim().to_string(),
            alias: alias.trim().to_string(),
        },
        None => InlineKeyword {
            tokens: s.trim().to_string(),
            alias: s.trim().to_string(),
        },
    }
}

/// Translate the raw matched-token string from sherpa-onnx into the
/// human-readable alias the user declared with `@`. Falls back to the
/// raw string when no match is found.
fn alias_for<'a>(matched: &'a str, inline: &'a [InlineKeyword]) -> &'a str {
    inline
        .iter()
        .find(|kw| kw.tokens == matched)
        .map(|kw| kw.alias.as_str())
        .unwrap_or(matched)
}

#[async_trait]
impl WakeDetector for SherpaKws {
    async fn wait_for_wake(
        &self,
        shutdown: Arc<AtomicBool>,
    ) -> Result<WakeEvent> {
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || wait_for_wake_blocking(&inner, &shutdown))
            .await
            .context("sherpa kws task panicked")?
    }
}

fn wait_for_wake_blocking(
    inner: &SherpaKwsInner,
    shutdown: &AtomicBool,
) -> Result<WakeEvent> {
    let started = Instant::now();
    let device = crate::audio_devices::find_input_device(inner.device_name.as_deref())?;
    let label = device.name().unwrap_or_else(|_| "?".into());
    info!("[sherpa kws] listening for wake word on input device: {}", label);

    let stream_cfg = device.default_input_config()?;
    let actual_rate = stream_cfg.sample_rate().0;
    let channels = stream_cfg.channels() as usize;
    let sample_format = stream_cfg.sample_format();
    let model_rate = inner.sample_rate as u32;
    info!(
        "[sherpa kws] format={:?}, channels={}, device_sample_rate={}, model_sample_rate={} \
         (callback will resample to model rate)",
        sample_format, channels, actual_rate, model_rate
    );
    info!(
        "[sherpa kws] active keywords: {} inline entry/entries (aliases: {:?})",
        inner.keywords_inline.len(),
        inner.keywords_inline.iter().map(|k| &k.alias).collect::<Vec<_>>()
    );

    // Producer/consumer split — same pattern as
    // `streaming_zipformer_microphone.rs` (the example uses `mpsc::channel`,
    // we use a `ringbuf` to avoid `Send` issues across the cpal callback).
    //
    // The ringbuf holds 30 s of mono f32 at the *model* rate (16 kHz),
    // not the device rate. The callback resamples from device rate to
    // model rate before pushing — see comment on the resampler below.
    let rb = HeapRb::<f32>::new(model_rate as usize * 30);
    let (mut prod, mut cons) = rb.split();

    // Shared stop flag + resampler state. The resampler carries
    // phase-coherent fractional-index state across callback
    // invocations, hence the Mutex. In practice the lock is
    // uncontended because only the cpal callback thread ever touches
    // it. The `running` flag lets us short-circuit the callback
    // safely when the stream is dropped (avoids pushing to a
    // dead ringbuf on shutdown).
    let running = Arc::new(AtomicBool::new(true));
    let resampler = Arc::new(Mutex::new(LinearResampler::new(actual_rate, model_rate)));

    let running_for_cb = running.clone();
    let resampler_for_cb = resampler.clone();
    let stream = match sample_format {
        SampleFormat::F32 => device.build_input_stream(
            &stream_cfg.into(),
            move |data: &[f32], _| {
                if !running_for_cb.load(Ordering::SeqCst) {
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
                if !running_for_cb.load(Ordering::SeqCst) {
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
                if !running_for_cb.load(Ordering::SeqCst) {
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

    // Stream creation — mirror of the example's
    // `kws.create_stream_with_keywords(extra_keywords)`. If no inline
    // entries were provided we just use `kws.create_stream()`.
    let kws = &inner.kws;
    let model_rate = inner.sample_rate;
    let kws_stream = if inner.keywords_inline.is_empty() {
        kws.create_stream()
    } else {
        let inline = inner
            .keywords_inline
            .iter()
            .map(|k| k.tokens.clone())
            .collect::<Vec<_>>()
            .join("/");
        kws.create_stream_with_keywords(&inline)
    };

    let mut last_level_log: Option<Instant> = None;
    let mut samples_total: usize = 0;
    let mut peak_seen: f32 = 0.0;
    let mut rms_acc: f32 = 0.0;
    let mut rms_count: u32 = 0;
    let mut last_speech_event: Option<Instant> = None;

    loop {
        // Check the shutdown flag at the top of every iteration.
        // The cpal stream Drop (when we return) closes the audio
        // device, so this is the only point at which we can
        // guarantee the OS releases the device handle. Without
        // this check, Ctrl+C cancels the async future but leaves
        // this `spawn_blocking` thread stuck on the empty-sleep
        // branch below, holding the device open and hanging the
        // process on exit.
        if shutdown.load(Ordering::SeqCst) {
            info!("[sherpa kws] shutdown requested, closing audio stream");
            bail!("sherpa kws shutdown requested");
        }
        let mut chunk = vec![0.0f32; 3200];
        let got = cons.pop_slice(&mut chunk);
        if got == 0 {
            std::thread::sleep(Duration::from_millis(10));
            maybe_log_silence(&started, &mut last_level_log, &samples_total, &mut peak_seen);
            continue;
        }
        chunk.truncate(got);
        // Update rolling audio level stats.
        let mut local_peak = 0.0f32;
        let mut local_sum_sq = 0.0f64;
        for &s in &chunk {
            let a = s.abs();
            if a > local_peak {
                local_peak = a;
            }
            local_sum_sq += (s as f64) * (s as f64);
        }
        samples_total += got;
        if local_peak > peak_seen {
            peak_seen = local_peak;
        }
        let rms_now = (local_sum_sq / got as f64).sqrt() as f32;
        rms_acc += rms_now;
        rms_count += 1;
        maybe_log_level(
            &started,
            &mut last_level_log,
            samples_total,
            peak_seen,
            rms_acc / rms_count as f32,
        );
        if rms_now > 0.02 {
            let should_log = match last_speech_event {
                None => true,
                Some(t) => t.elapsed() >= Duration::from_secs(2),
            };
            if should_log {
                last_speech_event = Some(Instant::now());
                info!(
                    "[sherpa kws] speech activity detected (chunk rms={:.4}, peak={:.4}). \
                     If the wake word doesn't fire after this, the keyword/pinyin may not match.",
                    rms_now, local_peak,
                );
            }
        } else {
            last_speech_event = None;
        }
        rms_acc = 0.0;
        rms_count = 0;

        // Main detection step — exact pattern from
        // rust-api-examples/examples/keyword_spotter.rs::detect_keywords.
        kws_stream.accept_waveform(model_rate, &chunk);
        while kws.is_ready(&kws_stream) {
            kws.decode(&kws_stream);
            if let Some(result) = kws.get_result(&kws_stream) {
                if !result.keyword.is_empty() {
                    // Reset before the next utterance, per the example.
                    kws.reset(&kws_stream);
                    drop(stream);
                    let elapsed = started.elapsed();
                    let alias = alias_for(&result.keyword, &inner.keywords_inline).to_string();
                    info!(
                        "[sherpa kws] wake hit: alias={} (raw={}) after {:.2}s listening (samples={}, peak={:.4})",
                        alias,
                        result.keyword,
                        elapsed.as_secs_f32(),
                        samples_total,
                        peak_seen,
                    );
                    return Ok(WakeEvent::wake(alias));
                }
            }
        }
    }
}

/// Open the mic, record `seconds` of audio, write it as 16 kHz / 16-bit
/// mono PCM to `output_path`, and return the peak / RMS of what was
/// recorded. This is the "what does the mic actually hear" tool —
/// when the KWS doesn't fire, run this and `aplay` the file to confirm
/// the wake word is being pronounced clearly enough.
pub fn dump_wake_test(
    cfg: &SherpaKwsConfig,
    output_path: &str,
    seconds: f32,
) -> Result<()> {
    use cpal::traits::StreamTrait;
    use hound::{SampleFormat as HFmt, WavSpec, WavWriter};

    let device = crate::audio_devices::find_input_device(cfg.device_name.as_deref())?;
    let label = device.name().unwrap_or_else(|_| "?".into());
    info!(
        "[wake-test] recording {:.1}s from input device: {} → {}",
        seconds, label, output_path,
    );
    let stream_cfg = device.default_input_config()?;
    let actual_rate = stream_cfg.sample_rate().0;
    let channels = stream_cfg.channels() as usize;
    let sample_format = stream_cfg.sample_format();
    info!(
        "[wake-test] device format={:?} channels={} rate={} (will resample to 16kHz mono for KWS)",
        sample_format, channels, actual_rate,
    );

    let n_samples = (actual_rate as f32 * seconds) as usize;
    let rb = HeapRb::<f32>::new(n_samples.max(8192));
    let (mut prod, mut cons) = rb.split();

    let stream = match sample_format {
        SampleFormat::F32 => device.build_input_stream(
            &stream_cfg.into(),
            move |data: &[f32], _| push_mono_f32(&mut prod, data, channels),
            err_fn,
            None,
        )?,
        SampleFormat::I16 => device.build_input_stream(
            &stream_cfg.into(),
            move |data: &[i16], _| push_mono_i16(&mut prod, data, channels),
            err_fn,
            None,
        )?,
        SampleFormat::U16 => device.build_input_stream(
            &stream_cfg.into(),
            move |data: &[u16], _| push_mono_u16(&mut prod, data, channels),
            err_fn,
            None,
        )?,
        other => bail!("unsupported sample format: {:?}", other),
    };
    stream.play()?;

    // Cap recording length even if the user is slow.
    let deadline = Instant::now() + Duration::from_secs_f32(seconds + 0.5);
    let mut captured: Vec<f32> = Vec::with_capacity(n_samples);
    while captured.len() < n_samples && Instant::now() < deadline {
        let mut chunk = vec![0.0f32; 1024];
        let got = cons.pop_slice(&mut chunk);
        if got == 0 {
            std::thread::sleep(Duration::from_millis(10));
            continue;
        }
        chunk.truncate(got);
        captured.extend_from_slice(&chunk);
    }
    drop(stream);

    // Resample to 16 kHz mono for the WAV (matches what the KWS model
    // actually receives). The audio level stats are computed on the
    // captured samples (device rate) and printed.
    let resampled = resample_linear(&captured, actual_rate, 16_000);
    let mut peak = 0.0f32;
    let mut sum_sq = 0.0f64;
    for &s in &resampled {
        let a = s.abs();
        if a > peak {
            peak = a;
        }
        sum_sq += (s as f64) * (s as f64);
    }
    let rms = (sum_sq / resampled.len().max(1) as f64).sqrt() as f32;

    let spec = WavSpec {
        channels: 1,
        sample_rate: 16_000,
        bits_per_sample: 16,
        sample_format: HFmt::Int,
    };
    let mut writer = WavWriter::create(output_path, spec)?;
    for &s in &resampled {
        let clipped = s.clamp(-1.0, 1.0);
        writer.write_sample((clipped * i16::MAX as f32) as i16)?;
    }
    writer.finalize()?;
    info!(
        "[wake-test] wrote {} samples ({:.2}s @ 16kHz) to {}, peak={:.4}, rms={:.4}",
        resampled.len(),
        resampled.len() as f32 / 16_000.0,
        output_path,
        peak,
        rms,
    );
    Ok(())
}

fn resample_linear(input: &[f32], from_rate: u32, to_rate: u32) -> Vec<f32> {
    if from_rate == to_rate || input.is_empty() {
        return input.to_vec();
    }
    let ratio = to_rate as f64 / from_rate as f64;
    let out_len = (input.len() as f64 * ratio) as usize;
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let src_idx = i as f64 / ratio;
        let lo = src_idx.floor() as usize;
        let hi = (lo + 1).min(input.len() - 1);
        let frac = (src_idx - lo as f64) as f32;
        out.push(input[lo] * (1.0 - frac) + input[hi] * frac);
    }
    out
}

fn push_mono_f32(prod: &mut impl Producer<Item = f32>, data: &[f32], channels: usize) {
    if channels == 1 {
        for &s in data {
            let _ = prod.try_push(s);
        }
        return;
    }
    for frame in data.chunks(channels) {
        let sum: f32 = frame.iter().copied().sum();
        let _ = prod.try_push(sum / channels as f32);
    }
}

fn push_mono_i16(prod: &mut impl Producer<Item = f32>, data: &[i16], channels: usize) {
    for frame in data.chunks(channels) {
        let sum: f32 = frame.iter().map(|&s| s as f32 / i16::MAX as f32).sum();
        let _ = prod.try_push(sum / channels as f32);
    }
}

fn push_mono_u16(prod: &mut impl Producer<Item = f32>, data: &[u16], channels: usize) {
    for frame in data.chunks(channels) {
        let sum: f32 = frame
            .iter()
            .map(|&s| ((s as f32) - 32768.0) / 32768.0)
            .sum();
        let _ = prod.try_push(sum / channels as f32);
    }
}

fn err_fn(err: cpal::StreamError) {
    warn!("[sherpa kws] stream error: {:?}", err);
}

const LEVEL_LOG_INTERVAL: Duration = Duration::from_secs(2);
const SILENCE_WARN_AFTER: Duration = Duration::from_secs(4);

fn maybe_log_level(
    started: &Instant,
    last: &mut Option<Instant>,
    total: usize,
    peak: f32,
    rms: f32,
) {
    let should_log = match *last {
        None => started.elapsed() >= LEVEL_LOG_INTERVAL,
        Some(t) => t.elapsed() >= LEVEL_LOG_INTERVAL,
    };
    if !should_log {
        return;
    }
    *last = Some(Instant::now());
    let secs = total as f32 / 16_000.0;
    let loud = rms > 0.005;
    if loud {
        info!(
            "[sherpa kws] audio level OK — total={} samples ({:.1}s), rms={:.4}, peak={:.4}",
            total, secs, rms, peak
        );
    } else {
        warn!(
            "[sherpa kws] audio level LOW — total={} samples ({:.1}s), rms={:.4}, peak={:.4}. \
             Speak louder, move the mic closer, or check PulseAudio source mute.",
            total, secs, rms, peak,
        );
    }
}

fn maybe_log_silence(
    started: &Instant,
    last: &mut Option<Instant>,
    total: &usize,
    peak: &mut f32,
) {
    let elapsed = started.elapsed();
    if elapsed < SILENCE_WARN_AFTER {
        return;
    }
    let should_log = match *last {
        None => true,
        Some(t) => t.elapsed() >= LEVEL_LOG_INTERVAL,
    };
    if !should_log {
        return;
    }
    *last = Some(Instant::now());
    if *total == 0 {
        warn!(
            "[sherpa kws] NO audio samples received after {:.1}s. Likely causes:\n\
               • PulseAudio source is muted — run `pactl set-source-mute alsa_input.usb-... 0`\n\
               • The source is suspended (idle) — try `pactl suspend-source alsa_input.usb-... 0`\n\
               • The wrong device is open — `pactl list sources short` to see what's available\n\
               • cpal opened the device but the source needs to be moved — `pavucontrol`",
            elapsed.as_secs_f32(),
        );
    } else if *peak < 0.001 {
        warn!(
            "[sherpa kws] mic is alive but very quiet — {} samples so far, peak={:.5}. \
             Try speaking closer to the mic or unmuting the source.",
            total, peak,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_inline_keyword_with_alias() {
        let kw = parse_inline_keyword("n ǐ h ǎo x iǎo j īn @你好小金");
        assert_eq!(kw.tokens, "n ǐ h ǎo x iǎo j īn");
        assert_eq!(kw.alias, "你好小金");
    }

    #[test]
    fn parse_inline_keyword_without_alias() {
        let kw = parse_inline_keyword("t uì ch ū r èn w u");
        assert_eq!(kw.tokens, "t uì ch ū r èn w u");
        assert_eq!(kw.alias, "t uì ch ū r èn w u");
    }

    #[test]
    fn alias_for_resolves_inline_match() {
        let inlines = vec![
            InlineKeyword {
                tokens: "n ǐ h ǎo x iǎo j īn".into(),
                alias: "你好小金".into(),
            },
            InlineKeyword {
                tokens: "t uì ch ū r èn w u".into(),
                alias: "退出任务".into(),
            },
        ];
        assert_eq!(alias_for("n ǐ h ǎo x iǎo j īn", &inlines), "你好小金");
        assert_eq!(alias_for("t uì ch ū r èn w u", &inlines), "退出任务");
        // Unknown token falls back to itself.
        assert_eq!(alias_for("x iǎo m ǐ", &inlines), "x iǎo m ǐ");
    }

    /// Regression test for the device-rate-vs-model-rate bug.
    ///
    /// The pre-fix KWS callback did downmix-only — it pushed raw
    /// 48 kHz samples into the ringbuf, then fed them to
    /// `kws_stream.accept_waveform(16_000, &chunk)`. The KWS model
    /// would treat the 48 kHz audio as 16 kHz, getting ~3x wrong
    /// time-base → no detection.
    ///
    /// The fix routes the callback through
    /// `LinearResampler(48_000, 16_000)`, so the consumer sees
    /// 16 kHz audio. This test pins the resampling ratio: feed
    /// 48_000 device-rate samples in, expect ~16_000 model-rate
    /// samples out.
    #[test]
    fn kws_callback_resamples_48k_to_16k() {
        use crate::cpal_capture::{downmix_f32, push_all, LinearResampler};
        use ringbuf::traits::Split;
        use ringbuf::HeapRb;

        let from_rate = 48_000u32;
        let to_rate = 16_000u32;
        // 1 s of "audio" at 48 kHz — just a ramp, content doesn't
        // matter for the ratio test.
        let n_device: usize = from_rate as usize;
        let device_samples: Vec<f32> = (0..n_device)
            .map(|i| (i as f32 / n_device as f32) * 2.0 - 1.0)
            .collect();

        // Run the exact same pipeline the live KWS callback uses.
        let rb = HeapRb::<f32>::new(to_rate as usize * 30);
        let (mut prod, mut cons) = rb.split();
        let mut resampler = LinearResampler::new(from_rate, to_rate);
        let mono = downmix_f32(&device_samples, /*channels=*/ 1);
        let resampled = resampler.resample(&mono);
        push_all(&mut prod, &resampled);
        drop(prod);

        let mut out = Vec::with_capacity(to_rate as usize);
        while let Some(s) = cons.try_pop() {
            out.push(s);
        }

        // Must be 16_000 ± a few (linear resampler carries a
        // boundary sample).
        let got = out.len();
        let expected = to_rate as usize;
        assert!(
            (got as i64 - expected as i64).abs() <= 2,
            "expected ~{expected} model-rate samples, got {got} (ratio drift)"
        );
    }

    /// Sanity check: at the same rate, the resampler is a
    /// no-op (1:1 passthrough) and chunk math is unchanged.
    #[test]
    fn kws_callback_passthrough_when_rates_match() {
        use crate::cpal_capture::{downmix_f32, push_all, LinearResampler};
        use ringbuf::traits::Split;
        use ringbuf::HeapRb;

        let samples: Vec<f32> = (0..3200).map(|i| i as f32).collect();
        let rb = HeapRb::<f32>::new(3200 * 2);
        let (mut prod, mut cons) = rb.split();
        let mut resampler = LinearResampler::new(16_000, 16_000);
        let mono = downmix_f32(&samples, 1);
        let resampled = resampler.resample(&mono);
        push_all(&mut prod, &resampled);
        drop(prod);

        let mut out = Vec::new();
        while let Some(s) = cons.try_pop() {
            out.push(s);
        }
        assert_eq!(out, samples);
    }
}
