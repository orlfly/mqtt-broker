//! Audio device enumeration and lookup.
//!
//! `log_audio_devices()` prints the cpal host + every input/output
//! device and its supported configurations at startup so users can
//! see what the agent is going to use.
//!
//! `find_input_device` / `find_output_device` are the single source
//! of truth for "given an `Option<&str>` from config, hand me back a
//! `cpal::Device`". They support:
//! - `None` or empty string → OS default
//! - exact name match (case-insensitive)
//! - substring match as a fallback
//!
//! On no match, the returned error lists every available device so
//! the user can see what they could have written instead.
//!
//! `log_diagnostic_commands()` additionally runs `arecord -l` and
//! `pactl list sources short` so the user can compare cpal's view to
//! the raw ALSA and PulseAudio views. The most common failure mode
//! is "the device shows up in `arecord -l` but cpal can't see it",
//! which means the user's `asound` config routes everything through
//! PulseAudio and the USB mic hasn't been loaded as a PulseAudio
//! source yet — the fix is to restart PulseAudio or run
//! `pactl load-module module-udev-detect`.
//!
//! Marked with `*` next to the device name when it's the OS default.

use anyhow::{bail, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait};
use cpal::Device;
use std::process::Command;
use tracing::{info, warn};

/// One card line from `arecord -l` / `aplay -l` output. The third
/// field is the human-friendly device name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlsaCard {
    pub card_num: u32,
    pub device_num: u32,
    pub name: String,
}

impl AlsaCard {
    pub fn display_label(&self) -> String {
        format!("hw:{},{}", self.card_num, self.device_num)
    }
}

/// Rough classification of a cpal device's name into one of three
/// buckets. Mirrors the labels used in
/// `voice-recognition/src/audio/capture.rs::AudioCapture::new`:
///   - names that look like raw ALSA hardware (`hw:N,M`, anything
///     with `CARD=N`) are reported as **Hardware**
///   - names that are sound-server proxies (`default`, `pulse`,
///     `pipewire`, `null`) are reported as **Virtual**
///   - everything else (long PulseAudio source names, friendly
///     description strings, USB product names) falls through to
///     **Unknown** — the user can usually tell by the name what
///     it actually is.
///
/// The classification is intentionally heuristic: the user
/// generally just wants to know "is this a real mic or a proxy I
/// need to follow back to the real device?".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceKind {
    /// Direct ALSA hardware (e.g. `hw:1,0`, anything tagged with
    /// `CARD=N`). Talks to the kernel directly; doesn't depend on
    /// PulseAudio / PipeWire being up.
    Hardware,
    /// Sound-server proxy (e.g. `default`, `pulse`, `pipewire`).
    /// These names only make sense while a PulseAudio / PipeWire
    /// daemon is running; on systems with multiple audio servers
    /// they may be ambiguous.
    Virtual,
    /// Couldn't tell from the name alone. Long PulseAudio source
    /// names like
    /// `alsa_input.usb-Generalplus_USB_MIC_Device-00.mono-fallback`
    /// land here — they are technically virtual (they go through
    /// PipeWire) but the heuristic doesn't want to be misleading.
    Unknown,
}

impl DeviceKind {
    pub fn label(self) -> &'static str {
        match self {
            DeviceKind::Hardware => "Hardware",
            DeviceKind::Virtual => "Virtual/Software",
            DeviceKind::Unknown => "Unknown",
        }
    }
}

pub fn classify_device_kind(name: &str) -> DeviceKind {
    // Heuristic hardware markers — match the reference project
    // (`voice-recognition/src/audio/capture.rs:33-37`).
    if name.contains("hw:") || name.contains("CARD") {
        return DeviceKind::Hardware;
    }
    // The well-known sound-server proxy names. `null` is the
    // ALSA null device (no-op sink/source) — almost never what
    // the user wants but we still want to label it virtual so
    // it stands out.
    if name == "default" || name == "pulse" || name == "pipewire" || name == "null" {
        return DeviceKind::Virtual;
    }
    DeviceKind::Unknown
}

pub fn log_audio_devices() {
    let host = cpal::default_host();
    let host_id = format!("{:?}", host.id());
    info!("[audio devices] cpal host = {}", host_id);

    let default_in = host
        .default_input_device()
        .and_then(|d| d.name().ok());
    let default_out = host
        .default_output_device()
        .and_then(|d| d.name().ok());

    match host.input_devices() {
        Ok(devs) => {
            let devices: Vec<_> = devs.collect();
            let count = devices.len();
            info!("[audio devices] input devices ({}):", count);
            for (i, d) in devices.iter().enumerate() {
                let name = d.name().unwrap_or_else(|_| "?".into());
                let marker = if default_in.as_deref() == Some(name.as_str()) {
                    "*"
                } else {
                    " "
                };
                let kind = classify_device_kind(&name);
                info!(
                    "[audio devices]   {} in[{}] = {:?}  [{}]",
                    marker,
                    i,
                    name,
                    kind.label()
                );
                log_supported_configs(d.supported_input_configs(), "    ");
            }
            if count == 0 {
                tracing::warn!("[audio devices] no input devices found");
            }
        }
        Err(e) => tracing::warn!("[audio devices] input_devices() failed: {e}"),
    }

    match host.output_devices() {
        Ok(devs) => {
            let devices: Vec<_> = devs.collect();
            let count = devices.len();
            info!("[audio devices] output devices ({}):", count);
            for (i, d) in devices.iter().enumerate() {
                let name = d.name().unwrap_or_else(|_| "?".into());
                let marker = if default_out.as_deref() == Some(name.as_str()) {
                    "*"
                } else {
                    " "
                };
                let kind = classify_device_kind(&name);
                info!(
                    "[audio devices]   {} out[{}] = {:?}  [{}]",
                    marker,
                    i,
                    name,
                    kind.label()
                );
                log_supported_configs(d.supported_output_configs(), "    ");
            }
            if count == 0 {
                tracing::warn!("[audio devices] no output devices found");
            }
        }
        Err(e) => tracing::warn!("[audio devices] output_devices() failed: {e}"),
    }

    if let Some(name) = &default_in {
        info!("[audio devices] default input  = {:?}", name);
    } else {
        tracing::warn!("[audio devices] no default input device selected");
    }
    if let Some(name) = &default_out {
        info!("[audio devices] default output = {:?}", name);
    } else {
        tracing::warn!("[audio devices] no default output device selected");
    }
}

/// Resolve `requested` (from `voice.audio_input_device`) to a concrete
/// input device. See module docs for matching rules.
pub fn find_input_device(requested: Option<&str>) -> Result<Device> {
    let host = cpal::default_host();
    find_device(&host, requested, host.input_devices().ok(), "input")
}

/// Same as `find_input_device` but for the playback side.
pub fn find_output_device(requested: Option<&str>) -> Result<Device> {
    let host = cpal::default_host();
    find_device(&host, requested, host.output_devices().ok(), "output")
}

/// Pure matcher: returns the index in `names` that satisfies
/// `requested`, or a human-readable error. Match rules:
/// - `requested.is_none()` or empty / whitespace → `None` (caller
///   should fall back to the OS default — represented here as `Ok(None)`)
/// - exact case-insensitive match
/// - substring case-insensitive match
/// - ambiguous substring (more than one match) → `Err`
/// - no match → `Err` listing the candidates, plus (on Linux) a
///   diagnostic hint if the device is in `arecord -l` but not in
///   cpal's view.
pub fn match_device<'a>(
    requested: Option<&str>,
    names: &[&'a str],
    kind: &str,
) -> Result<Option<&'a str>> {
    let needle = match requested.map(str::trim).filter(|s| !s.is_empty()) {
        Some(n) => n,
        None => return Ok(None),
    };
    let lc = needle.to_ascii_lowercase();
    if let Some(n) = names.iter().find(|n| n.eq_ignore_ascii_case(needle)) {
        return Ok(Some(n));
    }
    let subs: Vec<&&str> = names
        .iter()
        .filter(|n| n.to_ascii_lowercase().contains(&lc))
        .collect();
    match subs.len() {
        1 => Ok(Some(subs[0])),
        n if n > 1 => bail!(
            "audio_{kind}_device {:?} matched multiple devices: {:?}. \
             Disambiguate by giving a more specific name.",
            needle,
            subs.iter().map(|s| **s).collect::<Vec<_>>(),
        ),
        _ => {
            let mut msg = format!(
                "audio_{kind}_device {:?} not found. Available {kind} devices: [{}]. \
                 Set this in broker.yaml under agent.channels.voice.",
                needle,
                names.iter().map(|n| format!("{:?}", n)).collect::<Vec<_>>().join(", "),
            );
            if cfg!(target_os = "linux") {
                if let Some(arec) = run_cmd("arecord", &["-l"]) {
                    let cards = parse_arecord_list(&arec);
                    if let Some(hint) = missing_in_cpal_hint(needle, names, &cards) {
                        msg.push_str(&hint);
                    }
                }
                if let Some(pactl_match) = pactl_match_for_cpal(needle) {
                    // If the user already configured the long pactl
                    // name, the "put the long name in broker.yaml"
                    // suggestion is useless. Detect that case and
                    // collapse to just the default-source path.
                    let already_using_pa_name = pactl_match
                        .eq_ignore_ascii_case(needle);
                    if already_using_pa_name {
                        msg.push_str(&format!(
                            "\n[hint] broker.yaml already uses the long PulseAudio name `{}`, \
                             which cpal's ALSA host cannot see. Try one of:\n  \
                               - run `pactl set-default-source {}` and change \
                                 `audio_input_device` in broker.yaml to null or \"default\"\n  \
                               - use the cpal-visible friendly name listed above in the \
                                 \"Available input devices\" line",
                            pactl_match, pactl_match,
                        ));
                    } else {
                        msg.push_str(&format!(
                            "\n[hint] pactl sees the device as `{}`. Either:\n  \
                               - put `{}` in broker.yaml's audio_input_device, OR\n  \
                               - run `pactl set-default-source {}` and set \
                                 audio_input_device to null/'default'",
                            pactl_match, pactl_match, pactl_match,
                        ));
                    }
                }
            }
            bail!("{msg}");
        }
    }
}

/// Resolve both devices once at startup and log which physical
/// device each request landed on. The KWS / capture / playback sites
/// will all log their own "using device" line later, so this is mostly
/// a quick sanity check that runs *before* model loading.
///
/// In addition to the "will use" line, we log the **selected
/// device's** supported capabilities (first 3 entries) and its
/// `default_input_config()` / `default_output_config()`. This is
/// the focused per-device diagnostic the reference project emits
/// at the end of its `AudioCapture::new` (see
/// `voice-recognition/src/audio/capture.rs:61-82`) — it answers
/// "what stream config will the agent actually open?" before the
/// models start loading, so a config mismatch surfaces at startup
/// rather than at the first capture.
pub fn log_selected_devices(
    input_requested: Option<&str>,
    output_requested: Option<&str>,
) -> Result<()> {
    let in_dev = find_input_device(input_requested)?;
    let in_label = in_dev.name().unwrap_or_else(|_| "?".into());
    let in_kind = classify_device_kind(&in_label);
    info!(
        "[audio devices] will use input  for KWS + capture: {:?} [{}] (config: {:?})",
        in_label, in_kind.label(), input_requested,
    );
    log_selected_device_capabilities(&in_dev, "input", /*limit=*/ 3);
    log_selected_default_config(&in_dev, "input");

    let out_dev = find_output_device(output_requested)?;
    let out_label = out_dev.name().unwrap_or_else(|_| "?".into());
    let out_kind = classify_device_kind(&out_label);
    info!(
        "[audio devices] will use output for TTS playback:    {:?} [{}] (config: {:?})",
        out_label, out_kind.label(), output_requested,
    );
    log_selected_device_capabilities(&out_dev, "output", /*limit=*/ 3);
    log_selected_default_config(&out_dev, "output");

    Ok(())
}

/// Focused "Selected device capabilities" log for the chosen
/// device — mirrors
/// `voice-recognition/src/audio/capture.rs:61-78`. The full
/// enumeration lives in `log_audio_devices`; this is just the
/// first N entries for the *device we actually picked*, so the
/// startup log is one screen, not three.
fn log_selected_device_capabilities(dev: &cpal::Device, kind: &str, limit: usize) {
    let configs: Vec<_> = match kind {
        "input" => match dev.supported_input_configs() {
            Ok(it) => it.collect(),
            Err(e) => {
                tracing::warn!("[audio devices] supported_input_configs() failed: {e}");
                return;
            }
        },
        "output" => match dev.supported_output_configs() {
            Ok(it) => it.collect(),
            Err(e) => {
                tracing::warn!("[audio devices] supported_output_configs() failed: {e}");
                return;
            }
        },
        _ => return,
    };
    if configs.is_empty() {
        info!("[audio devices]   selected {kind} device has no supported configs");
        return;
    }
    info!(
        "[audio devices]   selected {kind} device capabilities: {} formats available",
        configs.len()
    );
    for (idx, cfg) in configs.iter().take(limit).enumerate() {
        info!(
            "[audio devices]     [{}] {} ch, {} Hz, {:?}",
            idx,
            cfg.channels(),
            cfg.max_sample_rate().0,
            cfg.sample_format(),
        );
    }
    if configs.len() > limit {
        info!(
            "[audio devices]     ... ({} more not shown)",
            configs.len() - limit
        );
    }
}

/// "Device default input config: {config}" log for the chosen
/// device. Mirrors
/// `voice-recognition/src/audio/capture.rs:81-82`. After this line
/// runs, the user knows exactly what sample rate / channel count
/// / format the cpal stream will be opened with.
fn log_selected_default_config(dev: &cpal::Device, kind: &str) {
    let label = match kind {
        "input" => dev.default_input_config(),
        "output" => dev.default_output_config(),
        _ => return,
    };
    match label {
        Ok(cfg) => info!("[audio devices]   device default {kind} config: {:?}", cfg),
        Err(e) => tracing::warn!(
            "[audio devices]   default_{kind}_config() failed: {e}"
        ),
    }
}

fn find_device(
    host: &cpal::Host,
    requested: Option<&str>,
    devices: Option<impl Iterator<Item = Device>>,
    kind: &str,
) -> Result<Device> {
    let Some(mut iter) = devices else {
        bail!("{kind}_devices() failed on host {:?}", host.id());
    };
    let devices: Vec<Device> = iter.by_ref().collect();
    let names: Vec<String> = devices
        .iter()
        .map(|d| d.name().unwrap_or_else(|_| "?".into()))
        .collect();
    let name_refs: Vec<&str> = names.iter().map(String::as_str).collect();

    match match_device(requested, &name_refs, kind)? {
        None => {
            // OS default
            let dev = match kind {
                "input" => host.default_input_device(),
                "output" => host.default_output_device(),
                _ => unreachable!(),
            }
            .with_context(|| format!("no default {kind} device on host {:?}", host.id()))?;
            Ok(dev)
        }
        Some(matched_name) => {
            let idx = names.iter().position(|n| n == matched_name).unwrap();
            Ok(devices[idx].clone())
        }
    }
}

fn log_supported_configs(
    configs: Result<
        impl Iterator<Item = cpal::SupportedStreamConfigRange>,
        cpal::SupportedStreamConfigsError,
    >,
    indent: &str,
) {
    match configs {
        Ok(iter) => {
            // Dedupe by (format, rate-range, buffer-size). Many devices
            // (e.g. USB mics on Linux) report the same config for every
            // channel count from 1..=128; that's noise. The user just
            // needs to know what formats and rates the device accepts.
            use std::collections::BTreeMap;
            let mut summary: BTreeMap<(String, String, String), (u16, u16)> =
                BTreeMap::new();
            for cfg in iter {
                let fmt = cfg.sample_format().to_string();
                let min_rate = cfg.min_sample_rate().0;
                let max_rate = cfg.max_sample_rate().0;
                // ALSA drivers often report 1Hz..u32::MAX or
                // 4000Hz..u32::MAX because the kernel can resample.
                // Hide the absurd upper bound.
                let rate_str = if max_rate >= 100_000 {
                    if min_rate <= 1 {
                        "unspecified".to_string()
                    } else {
                        format!(">= {}Hz", min_rate)
                    }
                } else if min_rate == max_rate {
                    format!("{}Hz", min_rate)
                } else {
                    format!("[{}..{}]Hz", min_rate, max_rate)
                };
                let buf = match cfg.buffer_size() {
                    cpal::SupportedBufferSize::Range { min, max }
                        if *min == 1 && *max >= 1_000_000 =>
                    {
                        "unspecified".to_string()
                    }
                    cpal::SupportedBufferSize::Range { min, max } => {
                        format!("[{}..{}]", min, max)
                    }
                    cpal::SupportedBufferSize::Unknown => "unknown".to_string(),
                };
                let ch = cfg.channels();
                let key = (fmt, rate_str, buf);
                let entry = summary.entry(key).or_insert((ch, ch));
                entry.0 = entry.0.min(ch);
                entry.1 = entry.1.max(ch);
            }
            for ((fmt, rate, buf), (ch_min, ch_max)) in &summary {
                let ch_str = if ch_min == ch_max {
                    format!("{}", ch_min)
                } else {
                    format!("[{}..{}]", ch_min, ch_max)
                };
                info!(
                    "[audio devices]{indent}format={} channels={} rate={} buffer_size={}",
                    fmt, ch_str, rate, buf,
                );
            }
        }
        Err(e) => tracing::warn!("[audio devices]{indent}supported_configs() failed: {e}"),
    }
}

/// Cheap runtime snapshot of the input device state.
///
/// `log_audio_devices()` and `log_diagnostic_commands()` are
/// startup-time only: they dump everything once and you have to
/// re-run the agent to see a new state. When the bug is "device
/// disappears from cpal's view mid-session" (the USB MIC + PipeWire
/// flakiness we keep hitting), we need to see **what cpal saw at
/// the exact moment the wake/capture failed**, not what it saw
/// 30 seconds earlier at startup.
///
/// This function is intentionally light: it does NOT open any
/// streams, just enumerates `host.input_devices()` and shells out
/// to `pactl list sources short` once. Safe to call from inside
/// the wake-detector error path on every retry — the cost is
/// comparable to a single cpal enum (sub-millisecond) plus a
/// fork/exec of `pactl` (~5-10 ms on cold cache, less when the
/// shell is warm).
///
/// `label` is echoed in every log line so the user can grep the
/// timeline. Suggested labels: `"startup"`, `"after_wake_prompt"`,
/// `"before_capture"`, `"after_tts_response"`, `"after_wake_error"`.
/// Adding a new call site? Pick a short string that says **when**
/// you're taking the snapshot.
pub fn log_runtime_device_state(label: &str) {
    use cpal::traits::HostTrait;
    use std::time::SystemTime;
    let host = cpal::default_host();
    let ts = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);

    let default_in = host
        .default_input_device()
        .and_then(|d| d.name().ok());

    match host.input_devices() {
        Ok(devs) => {
            let names: Vec<String> = devs.filter_map(|d| d.name().ok()).collect();
            info!(
                "[device state @{} t={:.3}] cpal inputs ({}) = {:?}",
                label, ts, names.len(), names
            );
        }
        Err(e) => warn!(
            "[device state @{} t={:.3}] cpal input_devices() failed: {}",
            label, ts, e
        ),
    }
    if let Some(name) = &default_in {
        info!(
            "[device state @{} t={:.3}] cpal default input = {:?}",
            label, ts, name
        );
    } else {
        warn!(
            "[device state @{} t={:.3}] cpal default input = <none>",
            label, ts
        );
    }
    match run_cmd("pactl", &["list", "sources", "short"]) {
        Some(out) if !out.trim().is_empty() => {
            info!("[device state @{} t={:.3}] pactl sources:", label, ts);
            for line in out.lines() {
                info!("[device state @{} t={:.3}]   {}", label, ts, line);
            }
        }
        Some(_) => info!(
            "[device state @{} t={:.3}] pactl has no sources registered",
            label, ts
        ),
        None => info!(
            "[device state @{} t={:.3}] pactl unavailable (no PulseAudio/PipeWire?)",
            label, ts
        ),
    }
}

/// Run `arecord -l` and `pactl list sources short` / `sinks short`
/// and log the output. This lets users see what cpal reports next to
/// the raw ALSA and PulseAudio views, which is the fastest way to
/// figure out "device is in `arecord -l` but cpal can't see it"
/// (the answer is almost always: PulseAudio hasn't loaded it as a
/// source — fix with `pulseaudio -k` to restart the daemon).
///
/// If `input_requested` is `Some`, also fetches the detailed state
/// (mute, volume, suspend state) for the matching source so the
/// user can immediately see if the chosen source is muted or
/// suspended.
pub fn log_diagnostic_commands() {
    info!("[audio devices] --- ALSA raw view (`arecord -l`) ---");
    match run_cmd("arecord", &["-l"]) {
        Some(out) if !out.trim().is_empty() => {
            for line in out.lines() {
                info!("[audio devices]   {}", line);
            }
        }
        Some(_) => info!("[audio devices]   (no output — no capture hardware?)"),
        None => info!("[audio devices]   (arecord not in PATH)"),
    }
    info!("[audio devices] --- PulseAudio view (`pactl list sources short`) ---");
    match run_cmd("pactl", &["list", "sources", "short"]) {
        Some(out) if !out.trim().is_empty() => {
            for line in out.lines() {
                info!("[audio devices]   {}", line);
            }
        }
        Some(_) => info!("[audio devices]   (no sources registered with PulseAudio)"),
        None => info!("[audio devices]   (pactl not in PATH or PulseAudio not running)"),
    }
}

/// Parse `arecord -l` output into structured `AlsaCard` entries.
///
/// One physical line of input looks like:
/// ```text
/// card 1: Device [USB MIC Device], device 0: USB Audio [USB Audio]
/// ```
/// `card` and `device` are on the same line, so we parse them
/// together. The card's friendly name is the FIRST `[...]` group
/// after the card number (i.e. `[USB MIC Device]` in the example).
pub fn parse_arecord_list(output: &str) -> Vec<AlsaCard> {
    let mut out = Vec::new();
    for line in output.lines() {
        let trimmed = line.trim_start();
        let Some(rest) = trimmed.strip_prefix("card ") else {
            continue;
        };
        // rest = "1: Device [USB MIC Device], device 0: USB Audio [USB Audio]"
        let Some((card_str, after_card)) = rest.split_once(':') else {
            continue;
        };
        let Ok(card_num) = card_str.trim().parse::<u32>() else {
            continue;
        };
        // Friendly name: first [...] in the card part (before ", device ").
        let name = extract_first_bracketed(after_card).unwrap_or_else(|| "?".to_string());
        // Device: look for ", device N:" marker.
        let Some(idx) = after_card.find(", device ") else {
            continue;
        };
        let after = &after_card[idx + ", device ".len()..];
        let Some((dev_str, _)) = after.split_once(':') else {
            continue;
        };
        let Ok(device_num) = dev_str.trim().parse::<u32>() else {
            continue;
        };
        out.push(AlsaCard {
            card_num,
            device_num,
            name,
        });
    }
    out
}

/// Check whether `needle` matches any device name in the parsed
/// `arecord -l` output (case-insensitive).
pub fn find_in_arecord<'a>(
    cards: &'a [AlsaCard],
    needle: &str,
) -> Option<&'a AlsaCard> {
    let lc = needle.to_ascii_lowercase();
    cards
        .iter()
        .find(|c| c.name.to_ascii_lowercase().contains(&lc))
}

/// If `needle` is visible in `arecord -l` but missing from cpal's
/// device list, return a multi-line diagnostic hint suggesting the
/// user restart the sound server. Picks PulseAudio vs PipeWire
/// commands automatically.
pub fn missing_in_cpal_hint(
    needle: &str,
    cpal_names: &[&str],
    cards: &[AlsaCard],
) -> Option<String> {
    let card = find_in_arecord(cards, needle)?;
    if cpal_names
        .iter()
        .any(|n| n.eq_ignore_ascii_case(&card.name))
    {
        return None;
    }
    let mut s = String::new();
    let is_pipewire = detect_sound_server()
        .map(|name| name.contains("PipeWire"))
        .unwrap_or(false);
    if is_pipewire {
        s.push_str(&format!(
            "\n[hint] ALSA sees {} ({}), but cpal does not. PipeWire\n\
             hasn't loaded the device as a source. Try:\n  \
               systemctl --user restart pipewire pipewire-pulse   # restart PipeWire\n  \
               pw-cli ls Source | grep {}                         # check if PipeWire has it\n  \
               pactl set-default-source alsa_input.usb-...        # set as default\n\
             Then run the agent with `audio_input_device: \"default\"` in broker.yaml,\n\
             or pass the long name `alsa_input.usb-...` as the value.",
            card.name,
            card.display_label(),
            card.name,
        ));
    } else {
        s.push_str(&format!(
            "\n[hint] ALSA sees {} ({}), but cpal does not. PulseAudio\n\
             hasn't loaded the device as a source. Try:\n  \
               pulseaudio -k                              # restart the PulseAudio daemon\n  \
               pactl load-module module-udev-detect      # ask PulseAudio to rescan udev\n  \
               pactl load-module module-alsa-card device=hw:{}  # load the source explicitly",
            card.name,
            card.display_label(),
            card.card_num,
        ));
    }
    Some(s)
}

fn extract_first_bracketed(s: &str) -> Option<String> {
    let start = s.find('[')?;
    let rest = &s[start + 1..];
    let end = rest.find(']')?;
    Some(rest[..end].to_string())
}

fn run_cmd(prog: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(prog).args(args).output().ok()?;
    if !output.status.success() && output.stdout.is_empty() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Detect whether the user is running PipeWire (vs native PulseAudio).
/// Returns `Some("PipeWire 0.3.x")` etc. if a server is reachable, or
/// `None` if `pactl info` is unavailable / the server isn't running.
///
/// `pactl info` always includes a `Server Name:` line, and on PipeWire
/// it shows `(on PipeWire N.M.P)` after the PulseAudio version, which
/// is the easiest thing to grep for without having to call
/// `pw-cli` / `pactl list modules`.
pub fn detect_sound_server() -> Option<String> {
    let out = run_cmd("pactl", &["info"])?;
    for line in out.lines() {
        if let Some(rest) = line.strip_prefix("Server Name: ") {
            return Some(rest.trim().to_string());
        }
    }
    None
}

/// `pactl list sources` puts `Name:`, `State:`, `Mute:`, etc. on
/// tab-indented lines inside each `Source #N` block. Use this helper
/// everywhere we parse the detailed (non-`short`) view so the
/// parser stays consistent and we don't have to remember to
/// `trim_start()` at every call site.
fn pactl_field<'a>(line: &'a str, prefix: &str) -> Option<&'a str> {
    line.trim_start().strip_prefix(prefix).map(str::trim)
}

/// Look up the PulseAudio source whose description / name best
/// matches `needle`, and log its State / Mute / Volume fields. This
/// is the only way to tell from outside whether the chosen mic is
/// muted or in a suspended state — the short list doesn't show it.
///
/// `pactl list sources` output is the multiline `Name:` / `State:`
/// / `Mute:` / `Volume:` blocks per source. We extract them by
/// re-grouping lines that share a `Name:` header.
///
/// When no source matches, we log at INFO level (not WARN) with a
/// list of every source pactl actually sees, so the user can compare
/// against their configured name. The warning is reserved for the
/// real problems: MUTED or SUSPENDED. "pactl doesn't list this name"
/// is informational — cpal can still open the device through the
/// ALSA plugin even when the PulseAudio view is missing it.
pub fn log_pulseaudio_source_state(needle: &str) {
    let Some(out) = run_cmd("pactl", &["list", "sources"]) else {
        info!("[audio devices] (pactl unavailable — can't check source state)");
        return;
    };
    // First pass: collect every source block, so we can list them
    // all back to the user when nothing matched.
    #[derive(Default, Clone)]
    struct Source {
        name: String,
        state: String,
        mute: String,
        volume: String,
        desc: String,
    }
    let mut sources: Vec<Source> = Vec::new();
    let mut cur = Source::default();
    for line in out.lines() {
        if let Some(rest) = pactl_field(line, "Name: ") {
            if !cur.name.is_empty() {
                sources.push(std::mem::take(&mut cur));
            }
            cur.name = rest.to_string();
        } else if let Some(rest) = pactl_field(line, "State: ") {
            cur.state = rest.to_string();
        } else if let Some(rest) = pactl_field(line, "Mute: ") {
            cur.mute = rest.to_string();
        } else if let Some(rest) = pactl_field(line, "Volume: ") {
            cur.volume = rest.to_string();
        } else if let Some(rest) = pactl_field(line, "Description: ") {
            cur.desc = rest.to_string();
        }
    }
    if !cur.name.is_empty() {
        sources.push(cur);
    }

    // Lenient match. WirePlumber appends/removes suffixes like
    //   .mono-fallback / .stereo-fallback / .iec958
    // dynamically (they reflect the negotiated channel layout).
    // Matching on the full long name therefore breaks whenever
    // WirePlumber changes its mind. Normalize both sides and try
    // a few equivalence forms.
    let mut best: Option<&Source> = None;
    let mut best_kind: &str = "";
    for s in &sources {
        let pa = PulseAudioSource {
            name: s.name.as_str(),
            desc: s.desc.as_str(),
        };
        if let Some(kind) = pulseaudio_source_matches(needle, &pa) {
            if best.is_none() {
                best = Some(s);
                best_kind = kind;
            }
        }
    }
    if let Some(s) = best {
        info!(
            "[audio devices] PulseAudio source for {:?} → name={:?} desc={:?} state={} mute={} volume={} (matched by {})",
            needle, s.name, s.desc, s.state, s.mute, s.volume, best_kind,
        );
        if s.mute.eq_ignore_ascii_case("yes") {
            warn!(
                "[audio devices] source {:?} is MUTED. Run `pactl set-source-mute {:?} 0` to unmute.",
                needle, s.name,
            );
        }
        if s.state.eq_ignore_ascii_case("SUSPENDED") {
            // Note: this is often a *false positive* at startup.
            // PipeWire sources report SUSPENDED while idle, and
            // cpal's ALSA host moves the source to RUNNING the
            // moment it opens the capture stream. We log at WARN
            // (not INFO) because a genuine SUSPENDED state at
            // startup *is* worth investigating, but the message
            // makes the lazy-idle case explicit so the user
            // doesn't chase a non-issue.
            warn!(
                "[audio devices] source {:?} is SUSPENDED at startup. Usually this is the \
                 normal idle state on PipeWire and the KWS will wake it on first capture. If \
                 the wake word never fires, try `pactl suspend-source {:?} 0` to force RUNNING \
                 or open the device in pavucontrol.",
                s.name, s.name,
            );
        }
    } else {
        // No match — log all sources so the user can spot the
        // closest one (or a typo). PipeWire-aware hint.
        let is_pipewire = detect_sound_server()
            .map(|name| name.contains("PipeWire"))
            .unwrap_or(false);
        let mut msg = format!(
            "[audio devices] pactl has no source whose name or description contains {:?}. \
             This is informational: cpal may still open the device directly via ALSA even when \
             PulseAudio's view is missing it. pactl sources currently registered:",
            needle,
        );
        for s in &sources {
            msg.push_str(&format!("\n   - name={:?} desc={:?}", s.name, s.desc));
        }
        msg.push_str(
            "\n  Pick one of the names above for `audio_input_device`, or use a unique substring \
             (e.g. the USB vendor name).",
        );
        if is_pipewire {
            msg.push_str(
                "\n  On PipeWire, if the device is missing entirely, try:\n    \
                   systemctl --user restart pipewire pipewire-pulse wireplumber\n    \
                   pw-cli ls Source | grep -i <vendor>     # see what PipeWire sees\n    \
                   pactl set-default-source alsa_input.usb-<vendor>-00.mono-fallback",
            );
        } else {
            msg.push_str(
                "\n  On PulseAudio, if the device is missing entirely, try:\n    \
                   pulseaudio -k                                # restart the daemon\n    \
                   pactl load-module module-udev-detect        # ask PulseAudio to rescan udev",
            );
        }
        info!("{msg}");
    }
}

/// One row in `pactl list sources` output, with the fields we care
/// about. Used for both diagnostics and the source-state lookup.
pub struct PulseAudioSource<'a> {
    pub name: &'a str,
    pub desc: &'a str,
}

/// Return `Some(<match-kind>)` if `needle` should be considered a
/// match for this source. `match-kind` is a short label like
/// `"exact"` / `"substring"` / `"normalized"` used in the log line
/// so the user understands why the agent accepted a slightly
/// different name.
///
/// Matching rules (in order):
///   1. exact case-insensitive on `Name`
///   2. exact case-insensitive on `Description`
///   3. substring (case-insensitive) on `Name + " " + Description`
///   4. normalized match: strip WirePlumber channel-fallback
///      suffixes (`.mono-fallback`, `.stereo-fallback`, `.iec958`,
///      `.<ch>-fallback`) and any trailing `-NN` card index from
///      both sides, then re-try substring match. This is the rule
///      that handles
///        configured: alsa_input.usb-Generalplus_USB_MIC_Device-00.mono-fallback
///        pactl says:  alsa_input.usb-Generalplus_USB_MIC_Device-00.stereo-fallback
///      which the user cannot easily keep in sync.
pub fn pulseaudio_source_matches(needle: &str, source: &PulseAudioSource<'_>) -> Option<&'static str> {
    if needle.eq_ignore_ascii_case(source.name) {
        return Some("exact-name");
    }
    if !source.desc.is_empty() && needle.eq_ignore_ascii_case(source.desc) {
        return Some("exact-description");
    }
    let lc = needle.to_ascii_lowercase();
    let hay = format!("{} {}", source.name.to_ascii_lowercase(), source.desc.to_ascii_lowercase());
    if hay.contains(&lc) {
        return Some("substring");
    }
    // Normalized match — strip the dynamic suffix from both sides.
    let n_norm = normalize_pulseaudio_name(needle);
    let name_norm = normalize_pulseaudio_name(source.name);
    if !n_norm.is_empty()
        && !name_norm.is_empty()
        && (name_norm.eq_ignore_ascii_case(&n_norm)
            || name_norm.to_ascii_lowercase().contains(&n_norm.to_ascii_lowercase()))
    {
        return Some("normalized");
    }
    None
}

/// Strip the parts of a PulseAudio / PipeWire source name that
/// change at runtime, so the configured name in `broker.yaml`
/// keeps matching across WirePlumber updates / udev events:
///   - `<base>.mono-fallback` / `.stereo-fallback` / `.quad-fallback`
///     (any `.mono|stereo|quad|...-fallback` suffix)
///   - `<base>.iec958` etc.
///   - trailing `-NN` card index (`alsa_input.usb-Foo-00` → `alsa_input.usb-Foo`)
fn normalize_pulseaudio_name(name: &str) -> String {
    let mut s = name.to_string();
    // Drop WirePlumber channel-fallback suffixes.
    for suffix in [
        ".mono-fallback",
        ".stereo-fallback",
        ".quad-fallback",
        ".surround21-fallback",
        ".surround40-fallback",
        ".surround41-fallback",
        ".surround50-fallback",
        ".surround51-fallback",
        ".surround71-fallback",
        ".iec958",
    ] {
        if let Some(rest) = s.strip_suffix(suffix) {
            s = rest.to_string();
            break;
        }
    }
    // Drop trailing card index like `-00` / `-01` (just before the end).
    if let Some(idx) = s.rfind('-') {
        let tail = &s[idx + 1..];
        if !tail.is_empty() && tail.chars().all(|c| c.is_ascii_digit()) && tail.len() <= 2 {
            s.truncate(idx);
        }
    }
    s
}

/// `pactl list sources short` returns rows like
///   0   alsa_input.usb-Generalplus_USB_MIC_Device-00.mono-fallback   ...
/// The first column is the index, the second is the source NAME that
/// cpal would use through the `pulse` plugin. When the user configures
/// a friendly name like "USB MIC Device" but cpal only sees the long
/// pactl name, this returns the long name so we can suggest it in the
/// error message.
pub fn pactl_match_for_cpal(needle: &str) -> Option<String> {
    let out = run_cmd("pactl", &["list", "sources", "short"])?;
    let lc = needle.to_ascii_lowercase();
    for line in out.lines() {
        let mut cols = line.split('\t');
        let _idx = cols.next();
        let name = match cols.next() {
            Some(s) => s.trim(),
            None => continue,
        };
        // Match against the pactl name OR its description (the
        // "USB MIC Device" friendly name lives in the description
        // column on `pactl list sources` detailed view; in the short
        // view it doesn't, so we fall back to substring on the name).
        if name.to_ascii_lowercase().contains(&lc) {
            return Some(name.to_string());
        }
    }
    // Detailed view: scan `Description:` per source.
    if let Some(detailed) = run_cmd("pactl", &["list", "sources"]) {
        let mut cur_name = String::new();
        let mut cur_desc = String::new();
        for line in detailed.lines() {
            if let Some(rest) = pactl_field(line, "Name: ") {
                if !cur_name.is_empty()
                    && cur_desc.to_ascii_lowercase().contains(&lc)
                {
                    return Some(cur_name);
                }
                cur_name = rest.to_string();
                cur_desc.clear();
            } else if let Some(rest) = pactl_field(line, "Description: ") {
                cur_desc = rest.to_string();
            }
        }
        if !cur_name.is_empty() && cur_desc.to_ascii_lowercase().contains(&lc) {
            return Some(cur_name);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{
        classify_device_kind, match_device, normalize_pulseaudio_name, pactl_field,
        parse_arecord_list, find_in_arecord, missing_in_cpal_hint, pulseaudio_source_matches,
        AlsaCard, DeviceKind, PulseAudioSource,
    };

    #[test]
    fn none_means_os_default() {
        assert_eq!(match_device(None, &["default", "USB"], "input").unwrap(), None);
    }

    #[test]
    fn empty_string_means_os_default() {
        assert_eq!(match_device(Some(""), &["default"], "input").unwrap(), None);
        assert_eq!(match_device(Some("   "), &["default"], "input").unwrap(), None);
    }

    #[test]
    fn exact_match_is_case_insensitive() {
        assert_eq!(
            match_device(Some("usb mic device"), &["default", "USB MIC Device"], "input")
                .unwrap(),
            Some("USB MIC Device"),
        );
    }

    #[test]
    fn substring_match_when_no_exact() {
        assert_eq!(
            match_device(Some("USB"), &["default", "USB MIC Device", "pulse"], "input")
                .unwrap(),
            Some("USB MIC Device"),
        );
    }

    #[test]
    fn ambiguous_substring_is_an_error() {
        let err = match_device(
            Some("MIC"),
            &["USB MIC Device", "HDA Intel PCH Mic", "default"],
            "input",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("matched multiple devices"), "got: {err}");
        assert!(err.contains("USB MIC Device"));
        assert!(err.contains("HDA Intel PCH Mic"));
    }

    #[test]
    fn not_found_lists_all_candidates() {
        let err = match_device(Some("zzz"), &["default", "pulse", "USB"], "input")
            .unwrap_err()
            .to_string();
        assert!(err.contains("not found"));
        assert!(err.contains("\"default\""));
        assert!(err.contains("\"pulse\""));
        assert!(err.contains("\"USB\""));
    }

    #[test]
    fn parse_arecord_extracts_cards_and_names() {
        let sample = "\
**** List of CAPTURE Hardware Devices ****
card 0: PCH [HDA Intel PCH], device 0: ALC295 Analog [ALC295 Analog]
  Subdevices: 1/1
  Subdevice #0: subdevice #0
card 1: Device [USB MIC Device], device 0: USB Audio [USB Audio]
  Subdevices: 1/1
  Subdevice #0: subdevice #0
";
        let cards = parse_arecord_list(sample);
        assert_eq!(cards.len(), 2);
        assert_eq!(cards[0].card_num, 0);
        assert_eq!(cards[0].device_num, 0);
        assert_eq!(cards[0].name, "HDA Intel PCH");
        assert_eq!(cards[1].card_num, 1);
        assert_eq!(cards[1].device_num, 0);
        assert_eq!(cards[1].name, "USB MIC Device");
    }

    #[test]
    fn find_in_arecord_is_case_insensitive_substring() {
        let cards = vec![
            AlsaCard {
                card_num: 0,
                device_num: 0,
                name: "HDA Intel PCH".into(),
            },
            AlsaCard {
                card_num: 1,
                device_num: 0,
                name: "USB MIC Device".into(),
            },
        ];
        assert_eq!(
            find_in_arecord(&cards, "usb mic").map(|c| (c.card_num, c.device_num)),
            Some((1, 0))
        );
        assert_eq!(find_in_arecord(&cards, "Zzz"), None);
    }

    #[test]
    fn hint_appears_when_device_in_arecord_but_not_cpal() {
        let cards = vec![AlsaCard {
            card_num: 1,
            device_num: 0,
            name: "USB MIC Device".into(),
        }];
        let hint = missing_in_cpal_hint("USB MIC Device", &["default", "pulse"], &cards)
            .expect("should produce a hint");
        assert!(hint.contains("hw:1,0"));
        // The exact command depends on whether PulseAudio or
        // PipeWire is the active server. Either is fine.
        assert!(
            hint.contains("pulseaudio -k") || hint.contains("restart pipewire"),
            "hint should mention a way to restart the sound server, got: {hint}",
        );
    }

    #[test]
    fn no_hint_when_device_visible_in_cpal() {
        let cards = vec![AlsaCard {
            card_num: 1,
            device_num: 0,
            name: "USB MIC Device".into(),
        }];
        assert!(missing_in_cpal_hint("USB MIC Device", &["default", "USB MIC Device"], &cards).is_none());
    }

    #[test]
    fn no_hint_when_device_unknown_to_arecord() {
        assert!(missing_in_cpal_hint(
            "Zzz",
            &["default"],
            &[AlsaCard {
                card_num: 0,
                device_num: 0,
                name: "HDA Intel PCH".into(),
            }],
        )
        .is_none());
    }

    #[test]
    fn normalize_strips_mono_fallback_suffix() {
        // The suffix WirePlumber appends/removes at runtime.
        assert_eq!(
            normalize_pulseaudio_name(
                "alsa_input.usb-Generalplus_USB_MIC_Device-00.mono-fallback"
            ),
            "alsa_input.usb-Generalplus_USB_MIC_Device"
        );
        assert_eq!(
            normalize_pulseaudio_name(
                "alsa_input.usb-Generalplus_USB_MIC_Device-00.stereo-fallback"
            ),
            "alsa_input.usb-Generalplus_USB_MIC_Device"
        );
        assert_eq!(
            normalize_pulseaudio_name("alsa_input.usb-Foo-00.iec958"),
            "alsa_input.usb-Foo"
        );
    }

    #[test]
    fn normalize_preserves_names_without_suffix() {
        assert_eq!(
            normalize_pulseaudio_name("alsa_input.usb-Generalplus_USB_MIC_Device-00"),
            "alsa_input.usb-Generalplus_USB_MIC_Device"
        );
        assert_eq!(normalize_pulseaudio_name("default"), "default");
    }

    #[test]
    fn source_matches_exact_name() {
        let s = PulseAudioSource {
            name: "alsa_input.usb-Foo-00.mono-fallback",
            desc: "USB Foo",
        };
        assert_eq!(
            pulseaudio_source_matches("alsa_input.usb-Foo-00.mono-fallback", &s),
            Some("exact-name")
        );
    }

    #[test]
    fn source_matches_via_normalized_when_suffix_differs() {
        // The user's actual failure mode: config has .mono-fallback,
        // WirePlumber now reports .stereo-fallback. After the
        // WirePlumber update, the agent would have warned — this test
        // proves the lenient matcher bridges the gap.
        let s = PulseAudioSource {
            name: "alsa_input.usb-Generalplus_USB_MIC_Device-00.stereo-fallback",
            desc: "USB MIC Device",
        };
        assert_eq!(
            pulseaudio_source_matches(
                "alsa_input.usb-Generalplus_USB_MIC_Device-00.mono-fallback",
                &s
            ),
            Some("normalized")
        );
    }

    #[test]
    fn source_matches_via_substring() {
        // User wrote a friendlier substring; cpal / pactl still
        // accept it because the substring is unique.
        let s = PulseAudioSource {
            name: "alsa_input.usb-Generalplus_USB_MIC_Device-00.mono-fallback",
            desc: "USB MIC Device",
        };
        assert_eq!(
            pulseaudio_source_matches("usb-Generalplus", &s),
            Some("substring")
        );
    }

    #[test]
    fn source_does_not_match_unrelated_device() {
        let s = PulseAudioSource {
            name: "alsa_input.pci-0000_00_1f.3.analog-stereo",
            desc: "Built-in Audio Analog Stereo",
        };
        assert!(pulseaudio_source_matches("alsa_input.usb-Foo-00", &s).is_none());
    }

    #[test]
    fn classify_hardware_for_alsa_hw_names() {
        // Direct ALSA hardware references — `hw:N,M` and the
        // `CARD=N,DEV=M` form are always hardware.
        assert_eq!(classify_device_kind("hw:0,0"), DeviceKind::Hardware);
        assert_eq!(classify_device_kind("hw:1,0"), DeviceKind::Hardware);
        assert_eq!(
            classify_device_kind("plughw:CARD=Device,DEV=0"),
            DeviceKind::Hardware
        );
        // Substring match: a friendly name that embeds "CARD"
        // still gets classified as hardware.
        assert_eq!(
            classify_device_kind("HDA Intel PCH, ALC295 Analog, CARD=PCH"),
            DeviceKind::Hardware
        );
    }

    #[test]
    fn classify_virtual_for_sound_server_proxies() {
        // The four well-known proxy names. `null` is the ALSA null
        // device, which behaves like a no-op sink/source.
        assert_eq!(classify_device_kind("default"), DeviceKind::Virtual);
        assert_eq!(classify_device_kind("pulse"), DeviceKind::Virtual);
        assert_eq!(classify_device_kind("pipewire"), DeviceKind::Virtual);
        assert_eq!(classify_device_kind("null"), DeviceKind::Virtual);
    }

    #[test]
    fn classify_unknown_for_pulseaudio_source_names() {
        // The user's actual case: a long PulseAudio source name
        // doesn't match any hardware marker, so it falls into
        // `Unknown`. The name is technically a virtual device
        // (it goes through PipeWire), but the heuristic doesn't
        // want to be misleading about a name that's mostly
        // useful as an opaque identifier.
        assert_eq!(
            classify_device_kind("alsa_input.usb-Generalplus_USB_MIC_Device-00.mono-fallback"),
            DeviceKind::Unknown
        );
        // Friendly description strings (no `hw:`, no `CARD`, not
        // a proxy name) also fall into `Unknown`.
        assert_eq!(classify_device_kind("USB MIC Device"), DeviceKind::Unknown);
        assert_eq!(
            classify_device_kind("HDA Intel PCH, ALC295 Analog"),
            DeviceKind::Unknown
        );
    }

    #[test]
    fn classify_is_case_sensitive_for_proxy_names() {
        // The proxy-name check is exact — `DEFAULT` is treated as
        // `Unknown` because the matcher intentionally uses
        // `==` instead of `eq_ignore_ascii_case`. This matches
        // the reference's behavior, and avoids mis-classifying
        // user-named virtual sinks (e.g. "Default Mic") as the
        // OS default.
        assert_eq!(classify_device_kind("DEFAULT"), DeviceKind::Unknown);
        assert_eq!(classify_device_kind("Pulse"), DeviceKind::Unknown);
    }

    #[test]
    fn device_kind_label_is_stable() {
        // The label is part of the user-facing startup log; the
        // exact string is part of our public-ish contract.
        assert_eq!(DeviceKind::Hardware.label(), "Hardware");
        assert_eq!(DeviceKind::Virtual.label(), "Virtual/Software");
        assert_eq!(DeviceKind::Unknown.label(), "Unknown");
    }

    /// Regression test for the tab-indented `pactl list sources`
    /// parser. Real pactl puts `Name:`, `State:`, `Description:`
    /// on tab-indented lines inside each `Source #N` block, e.g.
    ///
    /// ```text
    /// Source #54
    ///         State: SUSPENDED
    ///         Name: alsa_input.usb-Generalplus_USB_MIC_Device-00.mono-fallback
    ///         Description: USB MIC Device Mono
    ///         Mute: no
    /// ```
    ///
    /// The pre-fix parser used `line.strip_prefix("Name: ")` which
    /// never matched the tab-indented line, leaving the sources
    /// list empty and reporting "no match" even when pactl clearly
    /// knew about the configured device. The fix routes all
    /// detailed-view field extractions through `pactl_field()`,
    /// which trims the leading whitespace.
    #[test]
    fn pactl_field_handles_tab_indented_lines() {
        // No leading whitespace — old behavior; should still work.
        assert_eq!(
            pactl_field("Name: foo", "Name: "),
            Some("foo"),
            "non-indented line must still parse"
        );
        // Tab-indented — the case the bug fix targets.
        assert_eq!(
            pactl_field("\tName: foo", "Name: "),
            Some("foo"),
            "tab-indented line must parse"
        );
        // Space-indented — also common from human-edited output.
        assert_eq!(
            pactl_field("  Description: USB MIC Device Mono", "Description: "),
            Some("USB MIC Device Mono"),
            "space-indented line must parse"
        );
        // Mixed leading whitespace.
        assert_eq!(
            pactl_field(" \t \tName: x", "Name: "),
            Some("x"),
            "mixed leading whitespace must parse"
        );
        // Wrong prefix — must return None.
        assert_eq!(
            pactl_field("\tState: SUSPENDED", "Name: "),
            None,
            "wrong prefix must not match"
        );
        // Empty value (technically possible on stub pactl impls).
        assert_eq!(
            pactl_field("\tName: ", "Name: "),
            Some(""),
            "empty value must still be detected"
        );
    }
}
