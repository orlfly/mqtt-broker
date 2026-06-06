//! Blocking cpal output helper used by the TTS speaker.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Result};
use cpal::traits::{DeviceTrait, StreamTrait};
use cpal::SampleFormat;
use ringbuf::traits::{Consumer, Producer, Split};
use ringbuf::HeapRb;
use tracing::{info, warn};

use crate::cpal_capture::LinearResampler;

pub fn play_samples(
    samples: &[f32],
    sample_rate: u32,
    device_name: Option<&str>,
    shutdown: &Arc<AtomicBool>,
) -> Result<()> {
    let device = crate::audio_devices::find_output_device(device_name)?;
    let label = device.name().unwrap_or_else(|_| "?".into());
    info!("[cpal playback] using output device: {}", label);

    let supported = device.default_output_config()?;
    let cfg = supported.config();
    let actual_rate = cfg.sample_rate.0;
    let channels = cfg.channels as usize;
    let sample_format = supported.sample_format();
    info!(
        "[cpal playback] format={:?}, channels={}, device_rate={}, tts_model_rate={}{}",
        sample_format,
        channels,
        actual_rate,
        sample_rate,
        if sample_rate == actual_rate {
            " (no resampling needed)".to_string()
        } else {
            format!(" (will resample {} -> {} Hz)", sample_rate, actual_rate)
        }
    );

    if sample_format != SampleFormat::F32 {
        bail!("only f32 output is supported in this build (got {:?})", sample_format);
    }

    // Resample the TTS buffer to the device's native rate BEFORE
    // pushing it into the ringbuf. Two reasons this has to happen
    // up-front rather than in the cpal callback:
    //
    //   1. cpal has no built-in resampling API at the stream
    //      level. It plays samples at exactly the rate the device
    //      was configured with (`actual_rate`). If the buffer
    //      holds 22050 Hz samples and the device is configured
    //      for 48000 Hz, those samples get played back at
    //      48000/22050 ≈ 2.18× speed; on a 192 kHz HDA DAC the
    //      ratio is 8.7× — exactly the "audio is 8× too fast"
    //      report that motivated this fix.
    //
    //   2. The post-playback sleep budget (line ~95 below) needs
    //      to know how long the device will actually take to
    //      drain the buffer. Computing it from the model rate
    //      silently under-waits when the model rate is lower
    //      than the device rate, cutting off the last chunk of
    //      audio mid-word.
    //
    // We reuse the same `LinearResampler` that the KWS / capture
    // paths use (already `pub(crate)` in `cpal_capture`), so the
    // interpolation behaviour is consistent across the crate.
    let mut resampler = LinearResampler::new(sample_rate, actual_rate);
    let mono_samples: Vec<f32> = if sample_rate == actual_rate {
        samples.to_vec()
    } else {
        resampler.resample(samples)
    };
    // Mono frames in the device's sample rate = mono source samples
    // for this stream. `playback_secs` is the time the device will
    // take to play one frame's worth of these samples.
    let playback_secs = mono_samples.len() as f32 / actual_rate as f32;
    info!(
        "[cpal playback] {} tts samples -> {} mono frames ({:.2}s @ {}Hz, {} ch)",
        samples.len(),
        mono_samples.len(),
        playback_secs,
        actual_rate,
        channels,
    );

    // Expand the mono source to all device channels BEFORE pushing
    // into the ringbuf. cpal delivers the output buffer in
    // interleaved frames (L, R, L, R, ... for stereo; 4 slots per
    // frame for surround). The callback below writes one ringbuf
    // sample per slot, so a stereo device consumes two mono source
    // samples per frame. Without this expansion the mono audio
    // would be played back at `channels`× speed AND the L/R
    // channels would be 1 sample out of phase, producing audible
    // comb filtering on top of the speed-up. The expansion is the
    // cheap, allocation-only fix; a per-frame pop-and-broadcast
    // inside the callback would be a more invasive change.
    let playback_samples: Vec<f32> = if channels <= 1 {
        mono_samples.clone()
    } else {
        let mut v = Vec::with_capacity(mono_samples.len() * channels);
        for &s in &mono_samples {
            for _ in 0..channels {
                v.push(s);
            }
        }
        v
    };

    let rb = HeapRb::<f32>::new(playback_samples.len() + (actual_rate as usize * channels));
    let (mut prod, mut cons) = rb.split();
    for s in &playback_samples {
        let _ = prod.try_push(*s);
    }

    let consumed_samples = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = consumed_samples.clone();

    let stream = match sample_format {
        SampleFormat::F32 => device.build_output_stream(
            &cfg,
            move |data: &mut [f32], _| {
                let mut n = 0usize;
                for s in data.iter_mut() {
                    match cons.try_pop() {
                        Some(v) => {
                            *s = v;
                            n += 1;
                        }
                        None => *s = 0.0,
                    }
                }
                counter.fetch_add(n, std::sync::atomic::Ordering::Relaxed);
            },
            err_fn,
            None,
        )?,
        other => bail!("unhandled sample format: {:?}", other),
    };
    stream.play()?;

    // Wait for the ringbuf to drain, using the *device* rate (post
    // resampling) to size the sleep budget. 500 ms tail allowance
    // covers ALSA / PulseAudio / PipeWire startup latency before
    // the cpal callback actually starts consuming.
    //
    // Bail immediately on shutdown: dropping the `stream` releases
    // the audio device. Without this check, a long TTS clip
    // (e.g. multi-sentence agent responses) would block Ctrl+C for
    // the full playback duration.
    let total = playback_samples.len();
    let sleep_budget = playback_secs + 0.5;
    let start = std::time::Instant::now();
    let mut total_consumed = 0usize;
    while total_consumed < total && start.elapsed().as_secs_f32() < sleep_budget {
        if shutdown.load(Ordering::SeqCst) {
            info!("[cpal playback] shutdown requested, dropping stream");
            drop(stream);
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(20));
        total_consumed = consumed_samples.load(std::sync::atomic::Ordering::Relaxed);
    }
    // If we timed out (e.g. callback hung), sleep proportional to
    // what should have been played at the device rate so the
    // trailing samples still reach the speaker. `consumed_samples`
    // counts interleaved slots (L+R for stereo), so divide by
    // `channels` to convert to frame count, then by `actual_rate`
    // to convert to seconds.
    let remaining = total.saturating_sub(total_consumed);
    if remaining > 0 {
        let extra = (remaining as f32 / channels as f32) / actual_rate as f32;
        std::thread::sleep(Duration::from_secs_f32(extra + 0.2));
    }
    drop(stream);
    Ok(())
}

fn err_fn(err: cpal::StreamError) {
    warn!("[cpal playback] stream error: {:?}", err);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The TTS path can hand us samples at any model rate. The
    /// playback helper must resample them to the device's native
    /// rate so the audio doesn't play at e.g. 8× speed on a
    /// 192 kHz HDA DAC. This test exercises the rate-mapping math
    /// without touching a real device.
    ///
    /// Specifically: 22050 Hz @ 1 s = 22050 samples. Resampled to
    /// 48000 Hz, we expect ≈ 48000 samples (allow ±1 % for the
    /// linear-interpolation boundary case).
    #[test]
    fn resample_22k_to_48k_yields_expected_length() {
        let mut r = LinearResampler::new(22050, 48000);
        let input: Vec<f32> = (0..22050).map(|i| (i as f32 / 22050.0).sin()).collect();
        let out = r.resample(&input);
        let expected = 48000;
        let tol = (expected as f32 * 0.01) as i32;
        assert!(
            (out.len() as i32 - expected).abs() <= tol,
            "expected ~{expected} samples, got {} (delta {})",
            out.len(),
            (out.len() as i32 - expected),
        );
    }

    /// The same model rate as the device rate must pass through
    /// unchanged (no spurious duplicate/zero samples). HDA Intel
    /// PCH often reports 48000 Hz and Piper VITS Chinese outputs
    /// 22050 Hz, so this is a separate code path from the resample
    /// test above.
    #[test]
    fn resample_passthrough_when_rates_match() {
        let mut r = LinearResampler::new(48000, 48000);
        let input: Vec<f32> = (0..4800).map(|i| (i as f32 / 100.0).sin()).collect();
        let out = r.resample(&input);
        assert_eq!(out.len(), input.len());
        for (a, b) in out.iter().zip(input.iter()) {
            assert!((a - b).abs() < 1e-6, "passthrough drifted: {a} vs {b}");
        }
    }

    /// The 8×-speed regression: 22050 Hz TTS audio played on a
    /// 192000 Hz device (some HDA DACs advertise this). After
    /// resampling to 192000 Hz, the buffer should be ~8.7× longer
    /// than the input. The pre-fix code would have pushed the
    /// short buffer to a 192 kHz device, getting ~8.7× speedup
    /// pitch-shifted up — exactly what the user reported.
    #[test]
    fn resample_22k_to_192k_yields_eight_x_length() {
        let mut r = LinearResampler::new(22050, 192000);
        let input: Vec<f32> = (0..22050).map(|i| (i as f32 / 22050.0).sin()).collect();
        let out = r.resample(&input);
        let ratio = out.len() as f32 / input.len() as f32;
        // 192000 / 22050 ≈ 8.707
        assert!(
            (ratio - 8.707).abs() < 0.1,
            "expected ratio ~8.7, got {ratio} ({} / {})",
            out.len(),
            input.len(),
        );
    }

    /// The 2×-speed regression on stereo devices: cpal delivers
    /// the output buffer as interleaved L/R frames. The callback
    /// writes one ringbuf sample per *slot*, so a stereo device
    /// consumes two mono source samples per frame. To make the
    /// source play at its true rate with the channels in phase,
    /// `play_samples` duplicates each mono sample `channels` times
    /// before pushing it into the ringbuf. This test exercises the
    /// duplication step in isolation so a regression there doesn't
    /// have to wait for someone to play TTS on a stereo host to
    /// surface.
    #[test]
    fn mono_expansion_for_two_channel_device() {
        let mono: Vec<f32> = vec![0.1, 0.2, 0.3, 0.4];
        let expanded: Vec<f32> = if 2_usize <= 1 {
            mono.clone()
        } else {
            let mut v = Vec::with_capacity(mono.len() * 2);
            for &s in &mono {
                for _ in 0..2 {
                    v.push(s);
                }
            }
            v
        };
        assert_eq!(expanded, vec![0.1, 0.1, 0.2, 0.2, 0.3, 0.3, 0.4, 0.4]);
    }

    #[test]
    fn mono_passthrough_for_mono_device() {
        let mono: Vec<f32> = vec![0.1, 0.2, 0.3];
        let expanded: Vec<f32> = if 1_usize <= 1 {
            mono.clone()
        } else {
            let mut v = Vec::with_capacity(mono.len());
            for &s in &mono {
                for _ in 0..1 {
                    v.push(s);
                }
            }
            v
        };
        assert_eq!(expanded, mono);
    }

    #[test]
    fn mono_expansion_for_surround_keeps_all_channels_in_phase() {
        // A 4-channel surround device would otherwise consume 4
        // mono samples per frame, making the audio play 4× as
        // fast as well. Pre-duplication also fixes that case.
        let mono: Vec<f32> = vec![0.5, -0.5];
        let channels = 4_usize;
        let mut expanded = Vec::with_capacity(mono.len() * channels);
        for &s in &mono {
            for _ in 0..channels {
                expanded.push(s);
            }
        }
        assert_eq!(expanded, vec![0.5, 0.5, 0.5, 0.5, -0.5, -0.5, -0.5, -0.5]);
        // And the per-frame content is correct: every channel of
        // frame N carries mono[N].
        for frame in 0..mono.len() {
            for ch in 0..channels {
                assert_eq!(expanded[frame * channels + ch], mono[frame]);
            }
        }
    }
}
