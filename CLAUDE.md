# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

Resona, a Rust rewrite of Spytify (renamed 2026-10-05), is a Spotify recorder for Windows: it captures the Spotify desktop client's audio, splits it into tracks, skips ads (Free tier) and writes tagged MP3 / WAV / FLAC files. New over the C# original (`E:\Workspace\spy-spotify`, port it from there): **Spotify Lossless** rips to FLAC 16/24-bit.

**`docs/PLAN.md` is the roadmap** (phases, open decisions, C# → Rust module mapping, the Lossless design). Read it before starting a phase and record findings (especially Phase 0 measurements) there.

## Commands

```powershell
cargo check  --workspace --all-targets
cargo clippy --workspace --all-targets
cargo test   --workspace
cargo test   -p resona-core analysis::tests::quantize_round_trips_and_saturates   # single test
cargo fmt    --all

cargo run -p resona                                                  # Slint app
cargo packager --release -p resona                                   # installer in target/packager (needs cargo-packager)
cargo run -p resona-core --example capture_spotify -- 30 out.flac   # capture + fidelity diagnostics, needs Spotify playing
cargo run -p resona-core --example record -- recordings 10          # real recording session, one FLAC per track
```

Toolchain pinned in `rust-toolchain.toml` (1.98.0, edition 2024).

## Layout

- `crates/core` (`resona-core`) — engine, no UI: `spotify` (process, window title, SMTC → `state::Status` → `monitor` events), `capture` (WASAPI process loopback thread), `analysis` (bit-transparency), `encode` (intermediate float WAV, then FLAC / WAV / MP3; `Quantizer` dithers only when reducing depth), `format`. Windows-only modules are behind `#[cfg(windows)]` so the crate still builds and tests elsewhere.
- Decision logic is kept pure and unit-tested (`spotify::title`, `spotify::state`, `analysis`); Windows modules only gather inputs. Keep new logic on the pure side.
- `metadata/` — `deezer` (blocking client, no key), `lookup` (pure: a Deezer hit is used only if title, artist and duration all agree), `tags` (`TrackTags`, written through lofty's concrete `VorbisComments`). Tags are best-effort: a failed lookup keeps Spotify's own details, never fails a recording.
- `recorder/` — `splitter` (pure: holds audio back 3 s and places each change at the frame it happened), `clock`, `naming`, `engine` (Windows: capture + monitor + splitter + FLAC encoder thread).
- `examples/` are the manual test harnesses against the real client: `capture_spotify`, `analyze_wav`, `spotify_probe`, `record`.
- `crates/app` (`resona`) — Slint UI. `ui/*.slint` compiled by `build.rs` with the `fluent` style. Slint is built with `renderer-software` + `backend-winit` on purpose (low memory, measured in WaveFlow); don't switch renderers without measuring.

## Invariants

- **Capture is per process tree, never system loopback.** `ProcessCapture::start` targets the *root* Spotify PID and always includes its children. Windows has no "this process alone" mode, and wasapi's `include_tree: false` means *everything except* the tree — never use it.
- **Capture format is 44.1 kHz f32 in the output device's own channel layout** (`audio_setup::OutputDevice::{channels, channel_mask}`), reduced to the front L/R pair by `format::front_stereo`. Never let Windows downmix (a 7.1 virtual-surround headset made every capture non-transparent) and never resample in our pipeline — both destroy Lossless bit-transparency. 44.1 kHz is Spotify's native rate in every tier.
- **Capture first, encode after the track ends** from the intermediate float WAV (`encode::wav::CaptureWav`). The capture thread only reads WASAPI packets and sends them on a channel: no encoding or file I/O there.
- **Audio enhancements are a hint, read from the registry** (`audio_setup::enhancements_active`, `FxProperties`; `IMMDevice::OpenPropertyStore` never shows them). The registry and the Settings switch have disagreed, and G HUB rewrites these properties: the per-track `analysis::Fidelity` is the verdict, never the registry.
- **Don't infer play state from packets.** Depending on the build, paused Spotify sends either nothing or a stream of silent packets (seen with the Store build); `silent`-flagged packets are turned into zeros. Silence proves nothing about bit-transparency (`BitAnalysis::is_silent`).
- **FLAC depth comes from `BitAnalysis::effective_depth`** (`BitDepth::Auto`): 16 when every sample is 16-bit exact, 24 otherwise. Quantization is rounding without dither, valid only at that depth.
- Code from `E:\Workspace\WaveFlow` may be copied here under MIT only when its sole author is the repo owner — check `git log --format='%an' -- <file>` first (see `docs/PLAN.md`). Copy modules; don't depend on `waveflow-core`.

Docs and user-facing strings are in French; code, comments and this file are in English.
