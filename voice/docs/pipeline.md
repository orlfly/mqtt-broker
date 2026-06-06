# Voice pipeline

End-to-end notes on how `mqtt-broker`'s voice channel turns microphone
audio into MQTT actions and back into spoken audio.

> This document is for operators debugging the agent on real hardware.
> It mirrors — but does not replace — the inline doc comments on the
> voice subsystem's hot paths.

---

## 1. Component map

| Crate | What lives here | Source |
| --- | --- | --- |
| `agent` | entrypoint, voice loop, follow-up classifier, LLM dispatch | `agent/src/main.rs`, `agent/src/voice_loop.rs` |
| `voice` | wake / capture / ASR / TTS traits + sherpa-onnx impls | `voice/src/lib.rs` |
| `common` | `VoiceChannelConfig` (loaded from `broker.yaml`) | `common/src/lib.rs` |
| `config/broker.yaml` | runtime config — wake word, models, devices | — |

The four voice primitives are exposed as `Arc<dyn Trait>`:

- `WakeDetector` — `SherpaKws` (KWS streaming model) or `ScriptedWakeDetector` (stub)
- `AudioCapture`  — `CpalCapture` (cpal + silero VAD) or `StubAudioCapture`
- `AsrTranscriber` — `SherpaStreamingAsr` (zipformer streaming) or `ScriptedAsr`
- `TtsSpeaker`   — `SherpaTts` (VITS-Piper) or `StubTts`

`voice::build_stack(&cfg)` returns a `VoiceStack` containing all four,
choosing sherpa vs stub impls from `cfg.engine`.

---

## 2. Startup order

`agent/src/main.rs` runs the following steps in order. Most failures
are caught eagerly so a misconfigured model path or missing device
surfaces at startup, not at the first user utterance.

1. `tracing_subscriber::fmt().with_env_filter(...)` — defaults to
   `info`. Set `RUST_LOG=debug` for verbose engine logs.
2. `Config::load_default()` — reads `config/broker.yaml`.
3. `build_agent(&cfg)` — wires zeroclaw (LLM provider, tool
   dispatcher, skills).
4. `voice::build_stack(voice_cfg)` — returns a `VoiceStack`. With
   `engine = "sherpa"` this loads all four model graphs.
5. *(sherpa engine only)* the agent then runs a **device diagnostic
   block** that prints everything an operator needs to confirm the
   right mic is wired up:

   ```text
   voice::audio_devices::log_audio_devices()       // cpal list + [Hardware]/[Virtual] labels
   voice::audio_devices::log_diagnostic_commands()  // arecord -l + pactl short
   voice::audio_devices::detect_sound_server()      // "PulseAudio" / "PipeWire" / "none"
   voice::audio_devices::log_pulseaudio_source_state(needle)  // PA state for configured input
   voice::audio_devices::log_selected_devices(in, out)        // capabilities + default config of the chosen devices
   ```

6. *(optional)* `VOICE_DUMP_WAKE_TEST=/tmp/wake.wav` short-circuits
   the rest: 3 s of mic audio dumped to a wav and the process exits.
   Used to confirm the right device + sample format before the
   full loop runs.
7. `FollowupClassifier` + `VoiceLoop::run()` start the main loop.

---

## 3. Device resolution

Configured in `broker.yaml` as `agent.channels.voice.audio_input_device`
and `audio_output_device`. Resolution order in
`voice::audio_devices::match_device` (`voice/src/audio_devices.rs:214`):

1. **`None` or empty string** → cpal default input/output (whatever
   the OS routes to — `default` source on PipeWire, `pulse` on
   PulseAudio, `plughw` on bare ALSA).
2. **Exact match** — case-insensitive, against
   `host.input_devices()` / `output_devices()`.
3. **Unique substring match** — case-insensitive, must match exactly
   one device. If the substring is ambiguous (matches >1 device),
   startup fails with a list of candidates.
4. **PulseAudio fallback** — when cpal can't see a name that
   `pactl list sources` knows about, the matcher
   (`pulseaudio_source_matches`) tries the configured name against
   pactl's view. If pactl knows it, we print a hint pointing the
   operator at the cpal-visible name.

Wired up in `find_input_device` / `find_output_device`
(`voice/src/audio_devices.rs:193-199`).

### 3.1 Name normalization

WirePlumber dynamically appends / removes channel-routing suffixes
based on what the device is currently configured to do:

- `.mono-fallback`, `.stereo-fallback`, `.quad-fallback`
- `.surround21-fallback` … `.surround71-fallback`
- `.iec958`

If a `pactl` source name is `…usb-Foo-00.mono-fallback` and cpal
sees it as `…usb-Foo-00.stereo-fallback` (because the user toggled
the channel count in pavucontrol), the substring matcher still
finds it via `normalize_pulseaudio_name()`
(`voice/src/audio_devices.rs`).

This is the *only* way to deal with the suffix flicker robustly
without a periodic re-probe.

### 3.2 Device classification

`DeviceKind::classify(name)` (`voice/src/audio_devices.rs:49-95`)
labels each device in `log_audio_devices`:

- **Hardware** — name contains `hw:` or `CARD=` (direct ALSA hardware).
- **Virtual/Software** — name is one of `default`, `pulse`, `pipewire`,
  `null` (sound-server proxies and the ALSA null sink).
- **Unknown** — anything else (most PulseAudio source names fall here).

The label is a debug aid only; the matcher does not use it.

---

## 4. Wake → record → ASR → TTS handoff

```
       mic PCM 16k
            │
   ┌────────▼─────────┐
   │  SherpaKws loop  │   (cpal native rate → 16k resample)
   └────────┬─────────┘
            │ WakeEvent { kind: Wake|Exit, keyword }
            │
   ┌────────▼─────────┐  capture_timeout_secs
   │  CpalCapture     │  silero VAD drives endpoint
   └────────┬─────────┘
            │ Vec<f32> 16k mono
            │
   ┌────────▼─────────┐
   │  SherpaStreamingAsr │  is_endpoint → reset()  (online mode)
   └────────┬─────────┘
            │ String
            │
   ┌────────▼─────────┐
   │  LLM dispatch     │  via zeroclaw provider + tool skills
   └────────┬─────────┘
            │ reply text
            │
   ┌────────▼─────────┐
   │  SherpaTts        │  generate_with_config(text, ..., |_,_| true)
   └────────┬─────────┘
            │ f32 samples @ 22050 Hz (model-native)
            │
   ┌────────▼─────────┐
   │  cpal playback    │  on `audio_output_device`
   └───────────────────┘
```

Key constraints:

- **ASR never starts before TTS finishes.** `voice_loop.rs` awaits
  the TTS future before opening the capture stream.
- **VAD endpoint → ASR reset.** `SherpaStreamingAsr` enables
  `enable_endpoint` and resets the stream the moment an endpoint is
  detected, so the next utterance starts with a clean state.
- **Capture timeout → TTS "等待超时".** After
  `capture_timeout_secs` of no speech, the loop fires a one-shot TTS
  and returns to wake-listening.
- **Exit wake word short-circuits.** If the KWS reports a
  `WakeEvent` with `kind = Exit` (matched against
  `exit_wake_words` case-insensitively), the loop plays
  `exit_prompt` (default: "好的,任务已退出") via TTS and returns to
  wake-listening.

---

## 5. Capture pipeline (cpal)

Owned by `voice/src/cpal_capture.rs`. The key invariants (mirrored
from `voice-recognition/src/audio/capture.rs` + `recorder.rs`):

1. **`Arc<AtomicBool> running`** is checked at the top of every
   callback. When set to `false`, the callback returns
   immediately — no further ringbuf pushes — so dropping the
   stream can never leave a callback pushing to a dead channel.
2. **Stream is opened at the device's *default* sample rate.** We
   do *not* force 16 kHz. The cpal callback resamples from the
   device rate to 16 kHz in-callback using a
   `LinearResampler` (phase-coherent across chunks, held behind
   `Arc<Mutex<_>>`).
3. **Three input formats supported** — F32, I16, U16.
   `downmix_f32` / `downmix_i16` / `downmix_u16` collapse any
   channel count to mono before resampling.
4. **`last_samples` (30 ms)** holds the most recent 16 kHz samples
   in a small ring buffer. When the VAD emits a segment, the
   trailing chunk is appended from `last_samples` — this avoids
   chopping off the last word when speech ends abruptly. (Same
   trick as `RecorderState.last_samples` in the reference.)
5. **Silero VAD timeout** is handled via `vad.flush()` + `vad.front()`
   when the wall-clock cap is reached, so we still emit whatever
   the VAD had buffered rather than dropping it.

### 5.1 Resampler caveats

- Linear interpolation, not sinc. Acceptable for ASR at 16 kHz
  (zipformer is trained on this), not great for music.
- The resampler is *not* sample-accurate across restarts; the
  linear-phase error is ~one sample at the output boundary. If
  you need to splice two captures cleanly, use `pcm-tools` or
  similar — not this resampler.
- A "hold-last-sample" boundary (the reference's
  `AudioFormatConverter::resample` does this) is **not**
  currently implemented. If you hear pops at capture boundaries,
  this is the place to add it.

---

## 6. Known PipeWire quirks

The reference setup is **PipeWire + WirePlumber + pulse-plugin** on
top of ALSA. Things that will bite you:

- **Dynamic channel-routing suffixes.** See §3.1. The matcher
  normalizes them; if you bypass the matcher and grep
  `cpal::Device::name()` directly, your code will break every
  time the user toggles a channel in pavucontrol.
- **`pulseaudio -k` is a no-op.** PipeWire doesn't run the legacy
  daemon. Use `systemctl --user restart pipewire pipewire-pulse
  wireplumber`. The diagnostic log
  (`log_pulseaudio_source_state`) tells you which one to restart.
- **cpal sees the PulseAudio name, not the friendly name.** The
  string in pavucontrol's input dropdown is *not* what
  `Device::name()` returns. The startup log
  (`log_audio_devices`) prints both, but `broker.yaml` must use
  the cpal-visible name.
- **Hot-plug drops the device silently.** If the USB mic is
  unplugged while the agent is running, the next KWS poll will
  error and the loop will exit. There is no auto-reconnect yet;
  restart the agent after replugging.
- **Default source changes per app.** PipeWire's default-source
  policy picks the most recently active source. If you have
  both a webcam mic and a USB mic, the agent will sometimes
  pick the webcam. Pin `audio_input_device` in `broker.yaml`
  to avoid this.

---

## 7. Diagnostic env vars

| Env var | Effect |
| --- | --- |
| `RUST_LOG` | Standard tracing filter. `debug` for sherpa-onnx internals, `trace` for the cpal callback loop. |
| `VOICE_DUMP_WAKE_TEST=/tmp/wake.wav` | Records 3 s of mic audio and exits. Use to confirm the right device + sample format before running the full loop. |
| `VOICE_DUMP_WAKE_SECS=5` | Override the duration of the wake-test recording. |
| `SHERPA_ONNX_LIB_DIR=/path/to/sherpa-onnx-v1.13.2-linux-x64-static-lib/lib` | Pre-built sherpa-onnx runtime. Skips the on-build download step. |
| `LD_LIBRARY_PATH` | May be needed alongside `SHERPA_ONNX_LIB_DIR` if the static lib has unresolved transitive deps. |

---

## 8. Test inventory

47 tests as of the latest commit:

- `voice::audio_devices::tests` — 18 unit tests covering
  `match_device`, `find_in_arecord`, `missing_in_cpal_hint`,
  `normalize_pulseaudio_name`, `pulseaudio_source_matches`,
  `DeviceKind::classify_device_kind`.
- `voice::cpal_capture::tests` — 8 unit tests covering
  `LinearResampler` (passthrough, interpolation, phase coherence,
  rate drop) and the three `downmix_*` helpers.
- `voice::sherpa_kws::tests` — 3 unit tests for `parse_inline_keyword`
  + `alias_for` resolution.
- `voice::traits::tests` — 5 unit tests for `WakeEvent::classify`.
- `voice::tests::silero_vad` (integration) — 2 tests with
  `lei-jun-test.wav` for the silero VAD round-trip.
- `voice::tests::mic_probe` (integration) — 1 test for mic probe
  energy metrics.
- `agent::voice_loop::tests` — 7 unit tests for the follow-up
  classifier and capture-timeout paths.

Run: `cargo test -p voice` (38 unit + 3 integration) and
`cargo test -p agent` (7 unit). Together: `cargo test --workspace
--exclude broker --exclude server --exclude api`.
