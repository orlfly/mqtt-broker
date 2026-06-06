//! Diagnostic probe: list output devices and report each device's
//! default config, so we can see what sample rate the OS picks
//! for the configured `audio_output_device`.
//!
//!   cargo run -p voice --example tts_device_diag

#![cfg(feature = "sherpa")]

use cpal::traits::{DeviceTrait, HostTrait};
use cpal::SampleFormat;

fn main() {
    let host = cpal::default_host();
    println!("[diag] cpal host = {:?}", host.id());
    match host.default_output_device() {
        Some(d) => {
            let name = d.name().unwrap_or_else(|_| "?".into());
            println!("[diag] default output device: {name:?}");
            match d.default_output_config() {
                Ok(cfg) => {
                    println!("[diag]   default config: {:?}", cfg);
                }
                Err(e) => println!("[diag]   default_output_config() failed: {e}"),
            }
            match d.supported_output_configs() {
                Ok(iter) => {
                    let mut i = 0;
                    for cfg in iter {
                        println!(
                            "[diag]   supported[{}]: ch={} rate=[{}..{}] format={:?}",
                            i,
                            cfg.channels(),
                            cfg.min_sample_rate().0,
                            cfg.max_sample_rate().0,
                            cfg.sample_format()
                        );
                        i += 1;
                        if i > 8 {
                            break;
                        }
                    }
                }
                Err(e) => println!("[diag]   supported_output_configs() failed: {e}"),
            }
        }
        None => println!("[diag] no default output device"),
    }

    println!("\n[diag] all output devices:");
    for (i, d) in host.output_devices().unwrap().enumerate() {
        let name = d.name().unwrap_or_else(|_| "?".into());
        println!("[diag]   out[{i}] = {name:?}");
        if let Ok(cfg) = d.default_output_config() {
            println!("[diag]     default: {:?}", cfg);
        }
        if let Ok(iter) = d.supported_output_configs() {
            let cfgs: Vec<_> = iter.take(4).collect();
            for c in &cfgs {
                println!(
                    "[diag]     sup ch={} rate=[{}..{}] fmt={:?}",
                    c.channels(),
                    c.min_sample_rate().0,
                    c.max_sample_rate().0,
                    c.sample_format()
                );
            }
        }
    }
}
