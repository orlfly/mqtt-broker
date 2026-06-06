//! End-to-end test that `SherpaKws::new` succeeds without an explicit
//! `keywords_file` by auto-detecting `<encoder_dir>/keywords.txt` (the
//! WenetSpeech 3.3M model ships one bundled).
//!
//! Skips silently when the KWS model is not on disk.

#![cfg(feature = "sherpa")]

use std::path::Path;

use voice::sherpa_kws::{SherpaKws, SherpaKwsConfig};

fn locate_kws_dir() -> Option<std::path::PathBuf> {
    let home = std::env::var("HOME").unwrap_or_default();
    for candidate in [
        format!("{home}/models/sherpa-onnx-kws-zipformer-wenetspeech-3.3M-2024-01-01"),
        format!("{home}/models/sherpa-kws-wenetspeech"),
        "/models/sherpa-onnx-kws-zipformer-wenetspeech-3.3M-2024-01-01".to_string(),
        "/models/sherpa-kws-wenetspeech".to_string(),
    ] {
        let p = std::path::PathBuf::from(&candidate);
        if p.join("encoder-epoch-12-avg-2-chunk-16-left-64.onnx").is_file() {
            return Some(p);
        }
    }
    None
}

fn kws_cfg(kws_dir: &Path) -> SherpaKwsConfig {
    SherpaKwsConfig {
        encoder: kws_dir
            .join("encoder-epoch-12-avg-2-chunk-16-left-64.onnx")
            .to_string_lossy()
            .to_string(),
        decoder: kws_dir
            .join("decoder-epoch-12-avg-2-chunk-16-left-64.onnx")
            .to_string_lossy()
            .to_string(),
        joiner: kws_dir
            .join("joiner-epoch-12-avg-2-chunk-16-left-64.onnx")
            .to_string_lossy()
            .to_string(),
        tokens: kws_dir.join("tokens.txt").to_string_lossy().to_string(),
        keywords_file: None, // ← exercise the auto-fallback
        keywords_inline: vec![],
        num_threads: 1,
        provider: "cpu".into(),
        debug: false,
        sample_rate: 16000,
        device_name: None,
    }
}

#[test]
fn kws_auto_falls_back_to_bundled_keywords_txt() {
    let Some(kws_dir) = locate_kws_dir() else {
        eprintln!("KWS model not found; skipping");
        return;
    };
    let cfg = kws_cfg(&kws_dir);
    // The model ships a keywords.txt — SherpaKws::new should pick it up
    // automatically and successfully create the KeywordSpotter.
    SherpaKws::new(cfg).expect("SherpaKws::new should succeed via auto-fallback");
    eprintln!(
        "KWS loaded with auto-detected keywords_file = {:?}",
        kws_dir.join("keywords.txt")
    );
}

#[test]
fn kws_errors_clearly_when_no_keywords_anywhere() {
    // Point the encoder at a directory that has encoder/decoder/joiner/
    // tokens but NO keywords.txt. The error message should be the
    // remediation hint, not the raw sherpa-onnx validation text.
    let Some(kws_dir) = locate_kws_dir() else {
        eprintln!("KWS model not found; skipping");
        return;
    };
    // Build a sibling dir that mirrors the model files but omits
    // keywords.txt.
    let scratch = std::env::temp_dir().join("voice-kws-no-keywords");
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).unwrap();
    for f in [
        "encoder-epoch-12-avg-2-chunk-16-left-64.onnx",
        "decoder-epoch-12-avg-2-chunk-16-left-64.onnx",
        "joiner-epoch-12-avg-2-chunk-16-left-64.onnx",
        "tokens.txt",
    ] {
        std::fs::copy(kws_dir.join(f), scratch.join(f)).unwrap();
    }

    let cfg = SherpaKwsConfig {
        encoder: scratch
            .join("encoder-epoch-12-avg-2-chunk-16-left-64.onnx")
            .to_string_lossy()
            .to_string(),
        decoder: scratch
            .join("decoder-epoch-12-avg-2-chunk-16-left-64.onnx")
            .to_string_lossy()
            .to_string(),
        joiner: scratch
            .join("joiner-epoch-12-avg-2-chunk-16-left-64.onnx")
            .to_string_lossy()
            .to_string(),
        tokens: scratch.join("tokens.txt").to_string_lossy().to_string(),
        keywords_file: None,
        keywords_inline: vec![],
        num_threads: 1,
        provider: "cpu".into(),
        debug: false,
        sample_rate: 16000,
        device_name: None,
    };

    let res = SherpaKws::new(cfg);
    let err = match res {
        Ok(_) => panic!("should fail without keywords"),
        Err(e) => e,
    };
    let msg = format!("{err:#}");
    assert!(
        msg.contains("kws_keywords_file") || msg.contains("keywords.txt"),
        "error should hint at the remediation, got: {msg}"
    );
    eprintln!("got expected error: {msg}");
}

#[test]
fn kws_falls_back_to_inline_keywords_as_buf_when_no_file() {
    // With explicit inline entries, the KWS should build successfully
    // even when the user hasn't set `kws_keywords_file` and there's
    // no bundled `keywords.txt` to auto-detect. The joined inline
    // pinyin tokens are pushed into `config.keywords_buf` so the C++
    // validation step passes.
    let Some(kws_dir) = locate_kws_dir() else {
        eprintln!("KWS model not found; skipping");
        return;
    };
    let mut cfg = kws_cfg(&kws_dir);
    cfg.keywords_inline = vec!["n ǐ h ǎo x iǎo j īn @你好小金".to_string()];
    SherpaKws::new(cfg).expect("KWS should build from inline entries when no file is available");
    eprintln!("KWS built from inline entries only (no keywords.txt fallback needed)");
}
