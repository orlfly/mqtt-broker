//! Offline TTS probe — generates speech for a Chinese phrase with the
//! bundled VITS Piper model (default), Kokoro, or Matcha. Run with:
//!
//!   cargo run -p voice --example tts_probe -- <text> [out.wav]
//!
//! Defaults: text = "等待超时", out = /tmp/tts_probe.wav.
//!
//! Env knobs (all optional):
//!   TTS_BACKEND = vits | kokoro | matcha    (default: vits)
//!   TTS_MODEL, TTS_TOKENS, TTS_DATA_DIR, TTS_VOICES, TTS_DICT_DIR,
//!   TTS_LEXICONS (comma-separated), TTS_SID (speaker id),
//!   TTS_VOCODER, TTS_RULE_FSTS (comma-separated)

#![cfg(feature = "sherpa")]

use sherpa_onnx::{
    GenerationConfig, OfflineTts, OfflineTtsConfig, OfflineTtsKokoroModelConfig,
    OfflineTtsMatchaModelConfig, OfflineTtsVitsModelConfig,
};
use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let text = args.get(1).cloned().unwrap_or_else(|| "等待超时".into());
    let out = args.get(2).cloned().unwrap_or_else(|| "/tmp/tts_probe.wav".into());

    let backend = std::env::var("TTS_BACKEND").unwrap_or_else(|_| "vits".into());

    let (config, default_sid) = match backend.as_str() {
        "vits" => {
            let model = std::env::var("TTS_MODEL").unwrap_or_else(|_| {
                "/home/xiao/models/vits-piper-zh_CN-huayan-medium/zh_CN-huayan-medium.onnx".into()
            });
            let tokens = std::env::var("TTS_TOKENS").unwrap_or_else(|_| {
                "/home/xiao/models/vits-piper-zh_CN-huayan-medium/tokens.txt".into()
            });
            let data_dir = std::env::var("TTS_DATA_DIR").unwrap_or_else(|_| {
                "/home/xiao/models/vits-piper-zh_CN-huayan-medium/espeak-ng-data".into()
            });
            println!("[tts_probe] backend= vits");
            println!("[tts_probe] model  = {model}");
            println!("[tts_probe] tokens = {tokens}");
            println!("[tts_probe] data   = {data_dir}");
            (
                OfflineTtsConfig {
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
                },
                0_i32,
            )
        }
        "kokoro" => {
            let model = std::env::var("TTS_MODEL")
                .unwrap_or_else(|_| "/home/xiao/models/kokoro-multi-lang-v1_1/model.onnx".into());
            let tokens = std::env::var("TTS_TOKENS")
                .unwrap_or_else(|_| "/home/xiao/models/kokoro-multi-lang-v1_1/tokens.txt".into());
            let voices = std::env::var("TTS_VOICES")
                .unwrap_or_else(|_| "/home/xiao/models/kokoro-multi-lang-v1_1/voices.bin".into());
            let data_dir = std::env::var("TTS_DATA_DIR").unwrap_or_else(|_| {
                "/home/xiao/models/kokoro-multi-lang-v1_1/espeak-ng-data".into()
            });
            let dict_dir = std::env::var("TTS_DICT_DIR")
                .unwrap_or_else(|_| "/home/xiao/models/kokoro-multi-lang-v1_1/dict".into());
            let lexicon = std::env::var("TTS_LEXICONS").unwrap_or_else(|_| {
                "/home/xiao/models/kokoro-multi-lang-v1_1/lexicon-us-en.txt,/home/xiao/models/kokoro-multi-lang-v1_1/lexicon-zh.txt".into()
            });
            let sid: i32 = std::env::var("TTS_SID")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(3);
            println!("[tts_probe] backend= kokoro");
            println!("[tts_probe] model  = {model}");
            println!("[tts_probe] tokens = {tokens}");
            println!("[tts_probe] voices = {voices}");
            println!("[tts_probe] data   = {data_dir}");
            println!("[tts_probe] dict   = {dict_dir}");
            println!("[tts_probe] lex    = {lexicon}");
            println!("[tts_probe] sid    = {sid}");
            (
                OfflineTtsConfig {
                    model: sherpa_onnx::OfflineTtsModelConfig {
                        kokoro: OfflineTtsKokoroModelConfig {
                            model: Some(model),
                            tokens: Some(tokens),
                            voices: Some(voices),
                            data_dir: Some(data_dir),
                            dict_dir: Some(dict_dir),
                            lexicon: Some(lexicon),
                            length_scale: 1.0,
                            ..Default::default()
                        },
                        num_threads: 2,
                        debug: true,
                        ..Default::default()
                    },
                    ..Default::default()
                },
                sid,
            )
        }
        "matcha" => {
            let acoustic = std::env::var("TTS_MODEL").unwrap_or_else(|_| {
                "/home/xiao/models/matcha-icefall-zh-baker/model-steps-3.onnx".into()
            });
            let vocoder = std::env::var("TTS_VOCODER").unwrap_or_else(|_| {
                "/home/xiao/models/vocos-22khz-univ/vocos-22khz-univ.onnx".into()
            });
            let tokens = std::env::var("TTS_TOKENS").unwrap_or_else(|_| {
                "/home/xiao/models/matcha-icefall-zh-baker/tokens.txt".into()
            });
            let dict_dir = std::env::var("TTS_DICT_DIR").unwrap_or_else(|_| {
                "/home/xiao/models/matcha-icefall-zh-baker/dict".into()
            });
            let lexicon = std::env::var("TTS_LEXICONS").unwrap_or_else(|_| {
                "/home/xiao/models/matcha-icefall-zh-baker/lexicon.txt".into()
            });
            let rule_fsts = std::env::var("TTS_RULE_FSTS").unwrap_or_else(|_| {
                "/home/xiao/models/matcha-icefall-zh-baker/phone.fst,/home/xiao/models/matcha-icefall-zh-baker/date.fst,/home/xiao/models/matcha-icefall-zh-baker/number.fst".into()
            });
            println!("[tts_probe] backend= matcha");
            println!("[tts_probe] acoustic= {acoustic}");
            println!("[tts_probe] vocoder = {vocoder}");
            println!("[tts_probe] tokens  = {tokens}");
            println!("[tts_probe] dict    = {dict_dir}");
            println!("[tts_probe] lex     = {lexicon}");
            println!("[tts_probe] rule_fsts= {rule_fsts}");
            (
                OfflineTtsConfig {
                    model: sherpa_onnx::OfflineTtsModelConfig {
                        matcha: OfflineTtsMatchaModelConfig {
                            acoustic_model: Some(acoustic),
                            vocoder: Some(vocoder),
                            lexicon: Some(lexicon),
                            tokens: Some(tokens),
                            dict_dir: Some(dict_dir),
                            length_scale: 1.0,
                            noise_scale: 0.667,
                            ..Default::default()
                        },
                        num_threads: 2,
                        debug: true,
                        ..Default::default()
                    },
                    rule_fsts: Some(rule_fsts),
                    ..Default::default()
                },
                0_i32,
            )
        }
        other => {
            eprintln!(
                "[tts_probe] unknown TTS_BACKEND={other:?} (expected 'vits', 'kokoro', or 'matcha')"
            );
            std::process::exit(1);
        }
    };

    println!("[tts_probe] text   = {text:?}");
    println!("[tts_probe] out    = {out}");

    let tts = OfflineTts::create(&config).expect("Failed to create OfflineTts");
    println!(
        "[tts_probe] model sample_rate = {} Hz, num_speakers = {}",
        tts.sample_rate(),
        tts.num_speakers()
    );

    for &speed in &[1.0_f32, 0.7, 0.5] {
        let gen_config = GenerationConfig {
            speed,
            sid: default_sid,
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
