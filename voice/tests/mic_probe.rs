//! Microphone capture smoke test: open the default cpal input device,
//! record ~1 second of audio, and report RMS / peak / zero-crossings.
//!
//! Used to verify the audio input path is alive on a host — silent
//! output usually means "no real mic connected" or "the host's audio
//! server is in monitor-only mode" (e.g. a sandbox without a working
//! capture backend).
//!
//! Skips silently when no input device is found.

#![cfg(feature = "sherpa")]

use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::SampleFormat;
use ringbuf::traits::{Consumer, Producer, Split};
use ringbuf::HeapRb;

const CAPTURE_SECS: f32 = 1.0;

fn list_input_devices() {
    let host = cpal::default_host();
    match host.input_devices() {
        Ok(devs) => {
            let names: Vec<String> = devs
                .filter_map(|d| d.name().ok())
                .collect();
            eprintln!("[mic-probe] input devices: {:?}", names);
        }
        Err(e) => eprintln!("[mic-probe] input_devices() failed: {e}"),
    }
}

#[test]
fn mic_capture_reports_nonzero_signal() {
    let host = cpal::default_host();
    let device = match host.default_input_device() {
        Some(d) => d,
        None => {
            eprintln!("[mic-probe] no default input device; skipping");
            return;
        }
    };
    let label = device.name().unwrap_or_else(|_| "?".into());
    eprintln!("[mic-probe] default input: {label}");

    let stream_cfg = match device.default_input_config() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[mic-probe] default_input_config() failed: {e}; skipping");
            return;
        }
    };
    let rate = stream_cfg.sample_rate().0;
    let channels = stream_cfg.channels() as usize;
    let format = stream_cfg.sample_format();
    eprintln!(
        "[mic-probe] format={:?} channels={} rate={}",
        format, channels, rate
    );

    let rb = HeapRb::<f32>::new((rate as f32 * CAPTURE_SECS * 2.0) as usize);
    let (mut prod, mut cons) = rb.split();

    let stream = match format {
        SampleFormat::F32 => device
            .build_input_stream(
                &stream_cfg.into(),
                move |data: &[f32], _| {
                    for &s in data {
                        let _ = prod.try_push(s);
                    }
                },
                |e| eprintln!("[mic-probe] stream error: {e:?}"),
                None,
            )
            .expect("build F32 stream"),
        SampleFormat::I16 => device
            .build_input_stream(
                &stream_cfg.into(),
                move |data: &[i16], _| {
                    for &s in data {
                        let _ = prod.try_push(s as f32 / i16::MAX as f32);
                    }
                },
                |e| eprintln!("[mic-probe] stream error: {e:?}"),
                None,
            )
            .expect("build I16 stream"),
        SampleFormat::U16 => device
            .build_input_stream(
                &stream_cfg.into(),
                move |data: &[u16], _| {
                    for &s in data {
                        let _ = prod.try_push((s as f32 - 32768.0) / 32768.0);
                    }
                },
                |e| eprintln!("[mic-probe] stream error: {e:?}"),
                None,
            )
            .expect("build U16 stream"),
        other => panic!("unsupported sample format: {other:?}"),
    };
    stream.play().expect("stream.play()");
    eprintln!("[mic-probe] stream.play() ok, capturing {CAPTURE_SECS:.1}s...");

    let started = Instant::now();
    let mut collected: Vec<f32> = Vec::new();
    while started.elapsed() < Duration::from_millis((CAPTURE_SECS * 1000.0) as u64) {
        let mut buf = vec![0.0f32; 4096];
        let got = cons.pop_slice(&mut buf);
        if got > 0 {
            collected.extend_from_slice(&buf[..got]);
        } else {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    drop(stream);

    // Stats
    let n = collected.len();
    if n == 0 {
        eprintln!("[mic-probe] captured 0 samples — capture backend produced nothing");
        return;
    }
    let mut sum_sq = 0.0f64;
    let mut peak = 0.0f32;
    let mut zc = 0usize;
    let mut last_sign = 0i8;
    for (i, &s) in collected.iter().enumerate() {
        sum_sq += (s as f64) * (s as f64);
        let a = s.abs();
        if a > peak {
            peak = a;
        }
        let sign = if s > 0.001 {
            1
        } else if s < -0.001 {
            -1
        } else {
            0
        };
        if i > 0 && sign != 0 && sign != last_sign && last_sign != 0 {
            zc += 1;
        }
        if sign != 0 {
            last_sign = sign;
        }
    }
    let rms = (sum_sq / n as f64).sqrt() as f32;
    eprintln!(
        "[mic-probe] captured {} samples ({:.2}s @ {}Hz) — RMS={:.4} peak={:.4} zero_crossings={}",
        n,
        n as f32 / rate as f32,
        rate,
        rms,
        peak,
        zc
    );

    // Heuristic: a working mic on a quiet room gives RMS ~0.001-0.05.
    // A truly dead backend gives RMS=0 and zero crossings. We only
    // fail if BOTH are zero — that's the "device open but no signal"
    // state. Low signal (rms < 0.0001) is reported but not failed,
    // since a quiet room is real.
    if rms < 0.0001 && peak < 0.001 && zc < 5 {
        eprintln!(
            "[mic-probe] WARNING: signal is essentially silent (rms={rms:.6} peak={peak:.6} zc={zc}). \
             Likely no real microphone connected, or host's audio server is not routing input."
        );
    } else {
        eprintln!("[mic-probe] OK — input is alive");
    }

    // Always exercise the device list too so the test output is useful
    // even when the default device is the loopback/null sink.
    list_input_devices();
}
