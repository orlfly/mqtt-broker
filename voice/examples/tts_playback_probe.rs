//! Plays a wav file using the project's cpal_playback::play_samples
//! helper, so we can verify the resampling rate logic in isolation.
//!
//!   cargo run -p voice --example tts_playback_probe -- <input.wav>
//!
//! Defaults: input = /tmp/tts_probe.wav.

#![cfg(feature = "sherpa")]

use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Instant;

use hound::WavReader;
use voice::cpal_playback::play_samples;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = args.get(1).cloned().unwrap_or_else(|| "/tmp/tts_probe.wav".into());
    println!("[playback_probe] reading {path}");

    let mut reader = WavReader::open(&path).expect("open wav");
    let spec = reader.spec();
    let samples: Vec<f32> = reader
        .samples::<i16>()
        .map(|s| s.expect("sample") as f32 / i16::MAX as f32)
        .collect();
    println!(
        "[playback_probe] wav: {} Hz, {} ch, {} samples ({:.3}s)",
        spec.sample_rate,
        spec.channels,
        samples.len(),
        samples.len() as f32 / spec.sample_rate as f32
    );

    // Sum down to mono if needed (piper is mono but be defensive).
    let mono: Vec<f32> = if spec.channels == 1 {
        samples
    } else {
        samples
            .chunks(spec.channels as usize)
            .map(|frame| frame.iter().sum::<f32>() / spec.channels as f32)
            .collect()
    };

    let shutdown = Arc::new(AtomicBool::new(false));
    let started = Instant::now();
    play_samples(&mono, spec.sample_rate, None, &shutdown).expect("play_samples");
    println!(
        "[playback_probe] playback returned after {:.2}s",
        started.elapsed().as_secs_f32()
    );
}
