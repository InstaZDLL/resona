# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

Rust rewrite of Spytify, a Spotify recorder for Windows: it captures the Spotify desktop client's audio, splits it into tracks, skips ads (Free tier) and writes tagged MP3 / WAV / FLAC files. New over the C# original (`E:\Workspace\spy-spotify`, port it from there): **Spotify Lossless** rips to FLAC 16/24-bit.

**`docs/PLAN.md` is the roadmap** (phases, open decisions, C# → Rust module mapping, the Lossless design). Read it before starting a phase and record findings (especially Phase 0 measurements) there.

## Commands

```powershell
cargo check  --workspace --all-targets
cargo clippy --workspace --all-targets
cargo test   --workspace
cargo test   -p spytify-core analysis::tests::quantize_round_trips_and_saturates   # single test
cargo fmt    --all

cargo run -p spytify                                                  # Slint app
cargo run -p spytify-core --example capture_spotify -- 30 out.flac   # capture spike, needs Spotify running
```

Toolchain pinned in `rust-toolchain.toml` (1.98.0, edition 2024).

## Layout

- `crates/core` (`spytify-core`) — engine, no UI: `spotify` (process, window title, SMTC → `state::Status` → `monitor` events), `capture` (WASAPI process loopback thread), `analysis` (bit-transparency), `encode` (intermediate WAV + FLAC), `format`. Windows-only modules are behind `#[cfg(windows)]` so the crate still builds and tests elsewhere.
- Decision logic is kept pure and unit-tested (`spotify::title`, `spotify::state`, `analysis`); Windows modules only gather inputs. Keep new logic on the pure side.
- `examples/` are the manual test harnesses against the real client: `capture_spotify` (Phase 0) and `spotify_probe` (Phase 1).
- `crates/app` (`spytify`) — Slint UI. `ui/*.slint` compiled by `build.rs` with the `fluent` style. Slint is built with `renderer-software` + `backend-winit` on purpose (low memory, measured in WaveFlow); don't switch renderers without measuring.

## Invariants

- **Capture is per process, never system loopback.** `ProcessCapture::start` targets the *root* Spotify PID with `include_tree = true`: audio is rendered by a Chromium child process.
- **Capture format is 44.1 kHz f32 in the output device's own channel layout** (`audio_setup::OutputDevice::{channels, channel_mask}`), reduced to the front L/R pair by `format::front_stereo`. Never let Windows downmix (a 7.1 virtual-surround headset made every capture non-transparent) and never resample in our pipeline — both destroy Lossless bit-transparency. 44.1 kHz is Spotify's native rate in every tier.
- **Capture first, encode after the track ends** from the intermediate float WAV (`encode::wav::CaptureWav`). The capture thread only reads WASAPI packets and sends them on a channel: no encoding or file I/O there.
- **Don't infer play state from packets.** Depending on the build, paused Spotify sends either nothing or a stream of silent packets (seen with the Store build); `silent`-flagged packets are turned into zeros. Silence proves nothing about bit-transparency (`BitAnalysis::is_silent`).
- **FLAC depth comes from `BitAnalysis::effective_depth`** (`BitDepth::Auto`): 16 when every sample is 16-bit exact, 24 otherwise. Quantization is rounding without dither, valid only at that depth.
- Code from `E:\Workspace\WaveFlow` may be copied here under MIT only when its sole author is the repo owner — check `git log --format='%an' -- <file>` first (see `docs/PLAN.md`). Copy modules; don't depend on `waveflow-core`.

Docs and user-facing strings are in French; code, comments and this file are in English.
