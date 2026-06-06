//! End-to-end test of the silero VAD path. We feed a real speech
//! recording through the same `VoiceActivityDetector` wiring that
//! `cpal_capture::capture_with_silero` uses, and assert the detector
//! surfaces real segments (and stays quiet on pure silence).
//!
//! Gated behind `--features sherpa`. Skips silently when the silero
//! model or a real speech wav is not on disk.

#![cfg(feature = "sherpa")]

use sherpa_onnx::{VadModelConfig, VoiceActivityDetector};
use voice::cpal_capture::CpalCaptureConfig;

const SAMPLE_RATE: u32 = 16000;
const SILERO_WINDOW: usize = 512;

fn build_vad(cfg: &CpalCaptureConfig) -> VoiceActivityDetector {
    let model_path = cfg
        .vad_model
        .as_deref()
        .expect("test requires vad_model path");
    let mut silero = sherpa_onnx::SileroVadModelConfig::default();
    silero.model = Some(model_path.to_string());
    silero.threshold = cfg.vad_threshold;
    silero.min_silence_duration = cfg.vad_min_silence_ms as f32 / 1000.0;
    silero.min_speech_duration = cfg.vad_min_speech_ms as f32 / 1000.0;
    silero.max_speech_duration = cfg.vad_max_speech_secs;
    silero.window_size = SILERO_WINDOW as i32;
    let vad_config = VadModelConfig {
        silero_vad: silero,
        ten_vad: Default::default(),
        sample_rate: SAMPLE_RATE as i32,
        num_threads: cfg.vad_num_threads,
        provider: Some("cpu".to_string()),
        debug: false,
    };
    VoiceActivityDetector::create(&vad_config, cfg.vad_buffer_secs).expect("create silero VAD")
}

fn silence(duration: f32) -> Vec<f32> {
    vec![0.0; (duration * SAMPLE_RATE as f32) as usize]
}

fn make_test_config(vad_path: &str) -> CpalCaptureConfig {
    CpalCaptureConfig {
        device_name: None,
        sample_rate: SAMPLE_RATE,
        vad_model: Some(vad_path.to_string()),
        vad_threshold: 0.5,
        vad_min_silence_ms: 300,
        vad_min_speech_ms: 100,
        vad_max_speech_secs: 5.0,
        vad_num_threads: 1,
        vad_buffer_secs: 60.0,
        rms_threshold: 0.01,
        silence_ms: 500,
        pre_speech_ms: 100,
    }
}

fn locate_vad() -> Option<String> {
    let home = std::env::var("HOME").unwrap_or_default();
    for path in [
        format!("{home}/models/silero_vad.onnx"),
        format!("{home}/models/silero_vad_v5.onnx"),
        "/models/silero_vad.onnx".to_string(),
        "/models/silero_vad_v5.onnx".to_string(),
        "models/silero_vad.onnx".to_string(),
    ] {
        if std::path::Path::new(&path).is_file() {
            return Some(path);
        }
    }
    None
}

/// Locate a real speech wav used for the VAD detection test. Without
/// one we skip — synthetic tones don't trigger silero (it's trained on
/// real voice).
fn locate_speech_wav() -> Option<(Vec<f32>, u32)> {
    let home = std::env::var("HOME").unwrap_or_default();
    for path in [
        "/tmp/vad-test/speech.wav".to_string(),
        format!("{home}/models/lei-jun-test.wav"),
        format!("{home}/vad-test/speech.wav"),
    ] {
        if let Some(w) = sherpa_onnx::Wave::read(&path) {
            let sr = w.sample_rate();
            // Wave::samples() already returns normalized f32 in [-1, 1] —
            // do NOT divide by 32768 again, that squashes everything to ~0.
            let samples: Vec<f32> = w.samples().to_vec();
            return Some((samples, sr as u32));
        }
    }
    None
}

fn run_vad(vad: &VoiceActivityDetector, samples: &[f32]) -> (usize, usize) {
    let mut buf = [0.0f32; SILERO_WINDOW];
    let mut pos = 0usize;
    let mut total_speech = 0usize;
    let mut segment_count = 0usize;
    while pos < samples.len() {
        let end = (pos + SILERO_WINDOW).min(samples.len());
        let got = end - pos;
        buf[..got].copy_from_slice(&samples[pos..end]);
        if got < SILERO_WINDOW {
            for s in &mut buf[got..] {
                *s = 0.0;
            }
        }
        vad.accept_waveform(&buf);
        while let Some(seg) = vad.front() {
            total_speech += seg.samples().len();
            segment_count += 1;
            vad.pop();
        }
        pos = end;
    }
    vad.flush();
    while let Some(seg) = vad.front() {
        total_speech += seg.samples().len();
        segment_count += 1;
        vad.pop();
    }
    (segment_count, total_speech)
}

#[test]
fn silero_vad_emits_segments_for_real_speech() {
    let Some(vad_path) = locate_vad() else {
        eprintln!("silero_vad.onnx not found; skipping");
        return;
    };
    let Some((samples, _sr)) = locate_speech_wav() else {
        eprintln!("no real speech wav available; skipping");
        return;
    };
    let cfg = make_test_config(&vad_path);
    let vad = build_vad(&cfg);

    let (segments, speech_samples) = run_vad(&vad, &samples);
    eprintln!(
        "silero VAD: {} windows ({:.2}s of input) -> {} segments, {} speech samples ({:.2}s)",
        samples.len() / SILERO_WINDOW,
        samples.len() as f32 / SAMPLE_RATE as f32,
        segments,
        speech_samples,
        speech_samples as f32 / SAMPLE_RATE as f32
    );
    assert!(segments > 0, "VAD should fire on real speech");
    assert!(
        speech_samples >= 1600,
        "total speech suspiciously short: {} samples",
        speech_samples
    );
}

#[test]
fn silero_vad_returns_no_segments_for_pure_silence() {
    let Some(vad_path) = locate_vad() else {
        eprintln!("silero_vad.onnx not found; skipping");
        return;
    };
    let cfg = make_test_config(&vad_path);
    let vad = build_vad(&cfg);

    let (segments, _) = run_vad(&vad, &silence(1.0));
    assert_eq!(segments, 0, "VAD should not fire on pure silence");
}
