<p align="center">
  <img src="crates/app/assets/icon.png" width="128" alt="Resona">
</p>

<h1 align="center">Resona</h1>

<p align="center">
  Records what the Spotify desktop client plays on Windows, one tagged file per track,
  and tells you whether each one came out <b>bit-perfect</b>.
</p>

---

Resona captures only Spotify's own audio (WASAPI per-process loopback), cuts it at
track changes, and saves FLAC, WAV or MP3 with tags from Spotify's catalogue and
Deezer. With Spotify Premium set to **Lossless**, a well set-up chain gives files
identical to what Spotify decoded, up to 24-bit / 44.1 kHz, and Resona checks every
sample to say so.

It is a Rust rewrite of [Spytify](https://github.com/jwallet/spy-spotify) (C#), with
a Slint interface.

## Features

- **Spotify's audio only**: other apps and system sounds never reach the files.
- **Lossless and verified**: each track is labelled *bit-perfect 16/24-bit*, *peaks
  limited by Spotify*, *slightly changed by Spotify* or *altered*, from an analysis of
  every sample.
- **Playlist mode**: paste a playlist, album or track link; Resona starts it in
  Spotify, records every track, pauses Spotify at the end and lists what is missing,
  with a retry. Private playlists work.
- **Exact tags**: album, album artist, track number, date, copyright and a 640 px cover
  of the release actually played, completed with genre, ISRC and label from Deezer.
- **Virtual cable** (optional, VB-Cable): Spotify plays silently into the cable while
  recording, so your headset's settings no longer matter and you can listen to
  something else; Resona can play it back to you if you want to hear it.
- **Checks before recording**: Spotify's volume, normalization, Automix and quality,
  Windows audio enhancements and the output device's sample rate.
- File naming by artist / album folders, track or recording order numbers; tracks
  already recorded are skipped (or replaced, or kept twice).
- Ads (Spotify Free) are never recorded and can be muted.
- French and English interface; notification area icon; update check.

## Requirements

- Windows 11 (the per-process capture needs Windows build 20348 or later).
- The Spotify desktop client. The Microsoft Store build ships `spotify_cli`, which
  Resona uses for playlist mode, exact tags and the volume check; without it,
  recording still works.

For lossless recordings, in Spotify: **Lossless** quality, volume at maximum,
normalization, equalizer, crossfade and Automix off. In Windows: audio enhancements
off and the output device at 44,100 Hz, or use the virtual cable option, which sets
the cable up by itself.

## Install

Download `resona_<version>_x64-setup.exe` from the
[releases](https://github.com/InstaZDLL/resona/releases). It installs for the current
user, without administrator rights.

## Build

```powershell
cargo run -p resona                                                   # the app
cargo test --workspace                                                # tests
cargo packager --release -p resona                                    # installer, needs cargo-packager
cargo run -p resona-core --example capture_spotify -- 30 capture.flac # capture + fidelity diagnostics
```

The toolchain is pinned in `rust-toolchain.toml`. Design notes and every measurement
behind the choices are in [`docs/PLAN.md`](docs/PLAN.md) (in French).

## Legal

Resona records the audio output, like any recorder plugged into the speakers; it does
not touch Spotify's DRM. Recording may be against Spotify's terms of use and your
local law: keep the files for your own use.

## License

[MIT](LICENSE)
