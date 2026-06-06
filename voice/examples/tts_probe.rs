//! Offline TTS probe — generates speech for a Chinese phrase with the
//! bundled Piper VITS model and writes a WAV to disk so we can verify
//! (a) the model returns audio at the expected rate, and (b) the
//! `speed` knob actually slows the audio down. Run with:
//!
//!   cargo run -p voice --example tts_probe -- <text> [out.wav]
//!
//! Defaults: text = "等待超时", out = /tmp/tts_probe.wav.

#![cfg(feature = "sherpa")]

use sherpa_onnx::{
    GenerationConfig, OfflineTts, OfflineTtsConfig, OfflineTtsVitsModelConfig,
};
use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let text = args.get(1).cloned().unwrap_or_else(|| "等待超时".into());
    let out = args.get(2).cloned().unwrap_or_else(|| "/tmp/tts_probe.wav".into());

    let model = std::env::var("TTS_MODEL")
        .unwrap_or_else(|_| "/home/xiao/models/vits-piper-zh_CN-huayan-medium/zh_CN-huayan-medium.onnx".into());
    let tokens = std::env::var("TTS_TOKENS")
        .unwrap_or_else(|_| "/home/xiao/models/vits-piper-zh_CN-huayan-medium/tokens.txt".into());
    let data_dir = std::env::var("TTS_DATA_DIR")
        .unwrap_or_else(|_| "/home/xiao/models/vits-piper-zh_CN-huayan-medium/espeak-ng-data".into());

    println!("[tts_probe] model  = {model}");
    println!("[tts_probe] tokens = {tokens}");
    println!("[tts_probe] data   = {data_dir}");
    println!("[tts_probe] text   = {text:?}");
    println!("[tts_probe] out    = {out}");

    let config = OfflineTtsConfig {
        model: sherpa_onnx::OfflineTtsModelConfig {
            vits: OfflineTtsVitsModelConfig {
                model: Some(model),
                tokens: Some(tokens),
                noise_scale: 0.667,
                noise_scale_w: 0.8,
                length_scale: 1.0,
                data_dir: Some(data_dir),
                ..Default::default()
            },
            num_threads: 2,
            debug: true,
            ..Default::default()
        },
        ..Default::default()
    };

    let tts = OfflineTts::create(&config).expect("Failed to create OfflineTts");
    println!(
        "[tts_probe] model sample_rate = {} Hz, num_speakers = {}",
        tts.sample_rate(),
        tts.num_speakers()
    );

    for &speed in &[1.0_f32, 0.7, 0.5] {
        let gen_config = GenerationConfig {
            speed,
            ..Default::default()
        };

        let started = Instant::now();
        let audio = tts
            .generate_with_config(&text, &gen_config, Some(|_samples: &[f32], _progress: f32| true))
            .expect("Generation failed");
        let elapsed = started.elapsed().as_secs_f32();

        let n = audio.samples().len();
        let sr = audio.sample_rate() as f32;
        let dur = n as f32 / sr;
        println!(
            "[tts_probe] speed={:.2} -> {} samples = {:.3}s @ {}Hz (gen {:.3}s, RTF={:.2})",
            speed, n, dur, sr as u32, elapsed, elapsed / dur
        );

        // Find the first/last non-silent sample so we can see the
        // "compressed" range when speed != 1.0.
        let first = audio.samples().iter().position(|&s| s.abs() > 0.01);
        let last = audio.samples().iter().rposition(|&s| s.abs() > 0.01);
        println!(
            "[tts_probe]   first/last non-silent sample: {:?} / {:?}",
            first, last
        );

        if (speed - 1.0).abs() < 1e-3 {
            if audio.save(&out) {
                println!("[tts_probe] saved 1.0x wav -> {out}");
            } else {
                eprintln!("[tts_probe] save failed");
            }
        } else {
            let alt = format!("/tmp/tts_probe_speed_{:.2}.wav", speed);
            if audio.save(&alt) {
                println!("[tts_probe] saved speed={:.2} wav -> {alt}", speed);
            }
        }
    }
}
