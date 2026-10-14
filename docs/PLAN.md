# Resona: plan for the Rust rewrite of Spytify

Resona records what the Spotify client for Windows plays, splits the stream into tracks, skips ads and writes tags. It is the rewrite of Spytify, a C# / WinForms app (`E:\Workspace\spy-spotify`), renamed on October 5, 2026. The rewrite keeps the principle (record the output, download nothing) and adds **Spotify Lossless** (FLAC up to 24-bit / 44.1 kHz).

## Decisions made

| Topic | Choice | Reason |
| --- | --- | --- |
| Interface | **Slint 1.18**, software renderer, Fluent style | Measured in `WaveFlow/prototypes/rust_slint_mini`: 10.5 MB private memory against 78 MB for egui. The app runs in the background all night. Form widgets ready, built-in i18n (`@tr`). |
| Capture | **WASAPI process loopback** on the `Spotify.exe` tree | Only Spotify's sound is captured, with no driver. But the capture goes through the chain of the device Spotify plays on (sample rate, enhancements, surround): hence the **virtual cable** option (VB-Cable, not bundled), which sends Spotify to the cable through the undocumented COM interface of `Router/AudioRouter.cs` (`core::routing`). |
| Encoding | Capture into a temporary 32-bit float WAV, encode when the track ends | The capture path stays simple; the encoder knows the analysis of the whole track (FLAC depth). It is also what the C# version does. |
| Platform | Windows only | Spotify desktop + WASAPI. Windows code is behind `#[cfg(windows)]`. |

## Open decisions

- **Minimum Windows version.** Process loopback is documented from Windows 10 build 20348. To check in practice on Windows 10 22H2 (build 19045).
- **Spotify Web API.** See [below](#spotify-web-api). Proposal: Deezer + Last.fm by default, Spotify API as an option later.

Settled: code taken from WaveFlow may be republished here under MIT. The files concerned (`metadata/deezer.rs`, `lastfm.rs`, `name_match.rs`, `album_match.rs`, `artwork/`, `tagio.rs`) have a single author, the owner of both repositories. Check again with `git log --format='%an' -- <file>` before copying any other file.

## Phases

### Phase 0 — Validate the capture (validated October 1, 2026)

Done: `resona-core::capture` (process loopback), `spotify::find_root_pid`, `analysis::BitAnalysis`, `encode::flac`, and the example:

```powershell
cargo run -p resona-core --example capture_spotify -- 30 test.flac
```

Questions to settle with the real client:

1. The capture works with `include_tree` on the root process (the audio is rendered by a child process).
2. **Volume**: does process loopback capture before or after the Windows session volume and Spotify's volume? Test 100 % then 80 %, and look at `bit-transparent`.
3. **Device sample rate**: default device at 48 kHz then at 44.1 kHz. Does Spotify or Windows resample before us?
4. **Muting ads**: if Spotify's audio session is muted (like `MainAudioSession` in C#), does the capture still get the sound? This decides how ads are skipped.
5. Pause: what does the capture receive?

Measurements of October 1, 2026 (Microsoft Store Spotify, Logitech PRO X headset):

| Point | Result |
| --- | --- |
| 1. Capture of the process tree | ✅ 30.0 s captured, no loss (`discontinuities 0`) |
| 2. Volume | The device volume (12 %, −30.7 dB) **is not** in the capture: you can listen at any volume. Spotify slider at maximum. |
| 2 bis. Channels | ✅ The headset (G HUB, virtual surround) is a **7.1** device. In stereo, Windows remixed the 8 channels: −13 dB and a recomputed signal. Fixed in code: capture in the device's layout, front pair kept (`format::front_stereo`). The 6 other channels are empty. `RESONA_FORCE_STEREO=1` reproduces the old capture. |
| 2 ter. Effects | ✅ **Cause of the remaining processing: Windows "Audio enhancements"** (*Device Default Effects*) on the headset. Turned off: `bit-transparent: yes, source is 16-bit` on a 16-bit Lossless track. Turning off only the G HUB equalizer was not enough: G HUB probably goes through these effects. Spotify's equalizer and normalization were already off (not tested separately). |
| 3. Device sample rate | ✅ **The device must run at 44.1 kHz.** At 48 kHz, the stream is resampled before the capture (81 % off the grid, mean deviation 0.25 step). Spotify Lossless never goes above 44.1 kHz, so nothing is lost by enforcing it. |
| 3 bis. 24-bit source | ⚠ "Heaven" (swim school, 24-bit Lossless) at 44.1 kHz: **almost transparent**. 13.6 % of samples are off the 24-bit grid, with a mean deviation of 0.02 to 0.09 step, and the first second is perfect. Analysis (`analyze_wav`): offsets at **every level** (not a limiter), **left and right on the same frames** (103,003 against ~13,600 by chance), in bursts of about 5 frames, of at most half a 24-bit step (≈ −144 dBFS). Signature of a near-zero signal added to the music, or of decoding in float. Not a child process on its own: excluding each one changes nothing. Automix off: unchanged. Not mid/side decoding (only 21 % identical offsets on left and right, correlation +0.27). **Conclusion: Spotify's internal 24-bit processing.** The 16-bit track was at 0.00 % with the same settings and the same chain, so neither Windows nor a stray stream is involved. Consequence: the 24-bit FLAC export rounds to the nearest integer, which recovers the source exactly as long as the deviation stays under ½ step. Only deviations of exactly ½ step (≈ 4 % of samples, loud passages) are ambiguous by 1 step. **Status: 24-bit "near transparent".** Phase 3: make `BitAnalysis` tell this case (max deviation ≤ ½ 24-bit step) from real processing. Note: wasapi's `include_tree: false` captures everything **except** the tree; Windows has no "this process alone" mode. A headset limited to 16-bit output does not prevent 24-bit capture. |
| 4. Muting ads | To test (`session_volumes` reads the state, muting is still needed) |
| 5. Pause | ⚠ Store Spotify **keeps sending silent packets** while paused. Silence is no longer counted as "transparent". |

Diagnostics added: `audio_setup::default_output_device` (device format) and `audio_setup::session_volumes` (volume and mute of Spotify's sessions), shown by `capture_spotify`.

Exit criterion, **met**: a track Spotify shows as "Lossless 16-bit" is captured bit-transparent and detected as 16-bit. Still to clarify without blocking what follows: the slight deviation of 24-bit sources (point 3 bis) and muting for ads.

### Phase 1 — Spotify detection (in progress)

Done:

- `spotify::process`: root process and list of PIDs.
- `spotify::window`: title of the Chromium window, even when hidden in the notification area.
- `spotify::smtc`: SMTC session.
- `spotify::title`: title parsing, with the cases of the C# tests.
- `spotify::state`: combines title, SMTC and audio activity, then computes events.
- `spotify::monitor`: thread polling every 500 ms.
- `spotify_probe` example to watch Spotify live.

Observed on October 1, 2026 with the Microsoft Store version of Spotify:

- The process is indeed called `Spotify.exe`. The SMTC id is `SpotifyAB.SpotifyMusic_zpdnekdrzrea0!Spotify` (`Spotify.exe` expected for the classic installer, not checked).
- When paused, the window title goes back to `Spotify Premium`, but **SMTC keeps the track** (title, artist, album, track number, duration, position) with `playing: false`. `state::classify` uses this: a pause only emits `PlayStateChanged`, no more false track change.
- On a track change, the window title changes first; SMTC follows at the next poll, and **not all at once**: the next track's duration was seen next to the previous title.

To do:

- Watch with `spotify_probe`: an ad (Free account), a podcast, the classic (non-Store) installer.
- Wire `audio_active` to the capture (phase 2).

Initial behaviour to port from `Spotify/SpotifyProcess.cs`, `SpotifyStatus.cs` and `SpotifyHandler.cs`:

- Title of the main window of the Spotify tree (`EnumWindows` + PID); `Spotify`, `Spotify Free` or `Spotify Premium` = idle or ad, otherwise `Artist - Title`.
- Events: track change, play/pause, position.
- Study SMTC (`GlobalSystemMediaTransportControlsSessionManager`): artist, title, album and duration without parsing the title. Keep the window title as a fallback, in particular to recognize ads.
- Take over the cases of `EspionSpotify.Tests/SpotifyStatusTests.cs` and `SpotifyProcessTests.cs`.
- `EspionSpotify.FakeSpotify` produces a fake `Spotify.exe`. It is recognized by `find_root_pid` and stays useful to test without the real client.

### Phase 2 — Recording engine (in progress)

Done, in `recorder/`:

- `splitter` (pure, tested): audio is held back 3 s before being written, and each change is placed at the sample where it happened, in the past. The cut is moved to the nearest silence (±300 ms from the title, ±100 ms when SMTC dated the start). Pause: digital silence is ignored and the file goes on. Ads and idle time are not written. Silence trimmed at both ends, kept inside. SMTC details received less than 1.5 s before a cut are ignored.
- `clock`: instant ↔ sample mapping, without extrapolating across a pause with no packets.
- `naming`: `Artist - Title.flac`, forbidden characters replaced, reserved names avoided, duplicates numbered.
- `engine` (Windows): capture + monitor + splitter, one temporary WAV per track in `.resona-tmp`, FLAC encoding on a separate thread, depth chosen by `analysis::Fidelity`. Capture resumes automatically if Spotify restarts. Partial tracks (joined midway, skipped) and too-short ones discarded by default.
- `monitor` sends timestamped events, plus `TrackStart`: the start of the track dated by the SMTC position and the time of its update.
- `record` example: `cargo run -p resona-core --example record -- <folder> [minutes] [--keep-partial]`.

First real run (October 1, 2026):

- Transitions and partial tracks correct (joined midway, interrupted at stop).
- **Pause taken for an ad**: Spotify's fade-out can still be heard at the next poll. Fixed in `state::classify`: idle title + SMTC paused on the same track = pause, even with sound.
- **Everything recorded "PROCESSED"**, over the whole length (99.4 % off the 16-bit grid according to `analyze_flac`): the **audio enhancements had been turned back on** (*Device Default Effects*, "Logitech HX2E Surround Sound Effect"), probably by G HUB, while an earlier capture showed them Off. **Reliable detection**, checked both ways on October 1, 2026: read `MMDevices\Audio\Render\{id}\FxProperties` in the registry. An effect is installed (`{d04e05a6…},1/2/5/6/7`) and `Disable_SysFx` (`{1da5d803…},5`) ≠ 1; Windows **deletes** this value when *Device Default Effects* is chosen. The `IMMDevice::OpenPropertyStore` API does not see these values (always `VT_EMPTY`). `RecorderEvent::CaptureStarted` carries the device and its `lossless_issues()`.
- Second run, effects off: **correct pause** (a single file) and durations right to within 1 s (silence removed at the ends). Slow and Heaven (24-bit): near transparent. PLAIINS (16-bit): bit-perfect. Tell Me I'm Wrong (16-bit): clean except the pause fade (0:21-0:22) and three bursts of ~200 samples. **Modern Dinosaur (16-bit): limiter**: 35.5 % of samples changed where the local level is within 3 dB of the maximum, none below (`analyze_flac`). Tell Me, just as loud, is untouched. To settle: an effect of the previous track (Slow, 24-bit, chained) or specific to the track.
- **Modern Dinosaur, started by hand**: still limited (first 2 seconds exact, then up to 58 % of samples changed in loud passages). **It is specific to the track**, not to the transition. New class `Fidelity::PeakLimited`: deviations only in 10 ms blocks within 3 dB of the maximum, and at least 1 s of quieter passages intact. Written in 24-bit.
- Third run (phase 4): Deezer tags found for the three complete tracks. **The registry contradicted itself**: `Disable_SysFx` missing (so "active") while Windows showed Off, and Tell Me I'm Wrong came out intact (610 samples out of 17.8 M). Reading the registry is therefore only a **hint** ("may be active"); the verdict is each track's `Fidelity`. `BitPerfect` now tolerates 1 changed sample in 10,000 (Spotify's micro-fades) and counts them (`touched`); `PeakLimited` requires ≥ 0.1 % of peaks changed.
- To do: check this setting again during a session (G HUB can turn it back on midway), and check whether changing it applies to the stream Spotify already opened or whether Spotify must be restarted. The virtual cable (phase 5) makes this moot when used.
- Settled: a pause or resume fade changes a few hundred samples. `Fidelity::BitPerfect { touched }` tolerates them (at most 1 in `TOUCHED_RATIO` = 10,000): the track stays "bit-perfect" at its original depth, these samples being rounded.

To check with the real client: accuracy of the cuts (start and end of each file), pause/resume, skipping a track, closing Spotify during a recording, changing the output device mid-session (the channel layout is read when the capture starts).

Initial plan:

Port `Watcher.cs`, `Recorder.cs` and `AudioSessions/AudioThrottler.cs`:

- State machine: current track, change → finalize the previous track and start the next one.
- **Pause / resume**: the monitor only emits `PlayStateChanged` (the content stays the same track). The recorder continues the same file. A pause followed by a skip does give a change.
- **SMTC metadata**: keep the `DetailsUpdated` that stayed stable (at least two polls), not the last ones received just before the track change, which may mix two tracks.
- Monitor `audio_active`: true when the capture receives **non-silent** packets (Store Spotify sends silence while paused). This is what reveals an ad behind a "Spotify Free" title.
- Pre-roll buffer (`AudioCircularBuffer`) so as not to lose the start of a track detected late. The title changes about 500 ms after the sound. `rtrb` ring buffer, like WaveFlow's audio engine.
- Silence removal at start and end (`SilenceAnalyzer`), minimum duration, already-recorded tracks skipped, order numbers, naming and folders (`Native/FileManager.cs`, tests `FileManagerTests.cs`).
- Ads: depending on point 4 of phase 0.

### Phase 3 — Output formats (done)

- `RecorderConfig::format` (`OutputFormat`): `Flac { depth }`, `Wav { depth }` (integer PCM), `Mp3 { kbps }` (CBR 128/160/192/256/320). `BitDepth::Auto` keeps the depth the capture holds (`Fidelity::lossless_depth`).
- `encode::Quantizer`: exact rounding at equal depth, **TPDF dither** when reducing (24-bit source written as 16-bit).
- `encode::mp3`: LAME through `mp3lame-encoder` (built from source, no DLL), maximum quality; LAME's **Info** header is rewritten at the start of the file, for gapless playback.
- Tags: Vorbis for FLAC, **ID3v2** for MP3 and WAV (`TrackTags::write`), cover art included.
- `record --format=flac|flac16|flac24|wav|wav16|wav24|mp3|mp3:<kbps>` example.

Initial plan:

- WAV: `hound`.
- MP3 CBR 128 to 320 kbps: `mp3lame-encoder` (LAME, like the C# version).
- FLAC: done. Add TPDF dither when the user forces 16-bit on a 24-bit source.
- Diagnosis of October 6 and 7, 2026: the missing `SEEKTABLE` was not why seeking failed (a valid Sidify FLAC has none either). `flacenc` 0.5.1's predictive modes produced 33 to 302 MB frames on 24-bit captures, sometimes undecodable. v0.2.0 turned these modes off and added an index, but PotPlayer still jumps back to 00:00 on "Mujo - Jazz Wizzard", even after remuxing without tags or index. The same PCM re-encoded by FFmpeg or libFLAC seeks correctly to 1:01; its decoded SHA-256 stays identical. The encoder now uses libFLAC, statically linked. `scripts/repair-flac-seektable.ps1` only adds the index to old files; it does not repair their audio frames.

### Phase 4 — Tags and metadata (in progress)

Done, in `metadata/`:

- `deezer`: blocking client taken from WaveFlow (refusals in the response body, rate limit), with track, album and cover art. **The advanced search `artist:"…" track:"…"` no longer returns anything** (checked October 1, 2026): free-text search, then strict selection.
- `lookup` (pure, tested): a result is kept only if the title (≥ 0.85, featuring ignored), the artist (one of those Spotify lists) and the duration (±3 s) agree; the album breaks ties. A remix or a live version is rejected. Incomplete tags are better than wrong ones.
- `name_match`: taken from WaveFlow, with `title_similarity`.
- `tags`: `TrackTags` built from Spotify and SMTC, completed by Deezer (album, album artist, track/total, disc, date, genres, ISRC, label, XL cover art). Written through lofty's concrete `VorbisComments` tag.
- Engine: FLAC encoded and tagged in `.resona-tmp`, then moved to the final folder. `RecorderEvent::Saved` tells where the tags came from (`TagsOutcome`).
- `tag_lookup "Artist" "Title" ["Album"] [seconds]` example.

Checked for real: Tell Me I'm Wrong, Modern Dinosaur, Heaven and Waterfalls (feat.) found, with ISRC, label, genres and cover art.

Left: Last.fm as a fallback (API key), MP3 (ID3v2) with phase 3, a setting to turn off online lookup.

Initial plan:

- `lofty`: ID3v2 for MP3, Vorbis comments and picture for FLAC. Write through the concrete tag, as WaveFlow's rule says (`edit::patch_file`), otherwise non-standard fields disappear.
- Take from `WaveFlow/src-tauri/crates/core`: `metadata/deezer.rs`, `metadata/lastfm.rs`, `metadata/name_match.rs`, `album_match.rs`, `artwork/`, `tagio.rs` (reliable writing on Windows: retries if an antivirus locks the file). Copy the modules rather than depend on `waveflow-core`, which pulls in sqlx, symphonia, libopus and wasmtime.
- Take over the cases of `MapperID3Tests.cs`, `LastFMAPITests.cs`.

### Phase 5 — Slint interface (in progress)

Choice (October 1, 2026): **a single screen** (Spotify's state, current track, button, list of the session's tracks with their quality) and **a side settings panel**. Options taken from the C# version: file layout, already-recorded tracks, ads, virtual cable (added October 2, 2026: it takes the headset out of the recorded chain). Dropped: free choice of the audio device, Spotify API credentials (Deezer), timer.

Done on the interface side (`crates/app`):

- `ui/app-window.slint`: header, "now playing" card (Spotify's state, title, artist, album, REC timer), warnings, Start/Stop button, session list with a quality badge per track, settings panel sliding in from the right.
- Texts in English in `@tr()`, translated by `translations/fr/LC_MESSAGES/resona.po`, built into the binary; the language changes live.
- `app.rs`: a monitor dedicated to the display (Spotify's state is visible even when not recording), engine events forwarded to the Slint loop, stopping (which waits for the last track to be encoded) off the UI thread, settings saved on every change, folder picker with `rfd`.
- `mapping.rs` (pure, tested): interface lists ↔ settings.
- Checked October 1, 2026: window in French, current track and changes followed.
- Virtual cable (October 2, 2026): `core::routing` reads and changes the output device chosen for Spotify (Settings → Volume mixer), 21H2+ and earlier interfaces; `audio_setup::spotify_output_device` checks that device, no longer the default device. Setting "Play Spotify through a virtual cable": during recording only, Spotify is sent to "CABLE Input" (given back to its device by `Drop`, even on error; an interrupted session is repaired at the next one). Automatic setup, without administrator rights, through `core::device_config` (`IPolicyConfig`, the Sound panel's interface): cable set to 44.1 kHz (VB-Cable installs at 48 kHz, checked October 2, 2026), and if the installer made the cable the default device, a warning with a button to go back to the real device. Option "Hear Spotify while recording" (`core::playback`): the app plays the capture back on the default device, instead of "Listen to this device"; when off, Spotify records silently and you can listen to something else. The driver is not bundled (VB-Audio licence "all rights reserved"), a link leads to the site. `route_spotify [cable|default]` example. **Measured October 3, 2026**: through the cable, headset deliberately with enhancements on, Oishii and Tell Me I'm Wrong (16-bit) come out bit-perfect 16-bit, clean cuts; yes, and? stays "peaks limited" (Spotify's file). Red herring the same day: a Spotify update had turned crossfade, Automix and normalization back on. Symptoms: cuts shifted by 0 to 1.25 s, start and end of tracks off the grid, everything "partial". Hence `BitAnalysis::fades_in` (first 2 seconds off the grid, the rest much less) and the `SpotifyFades` warning. Examples added: `align_flac` (offset between two recordings of a track), `record --cable|--no-cable`.

Done October 4, 2026: cover art of the current track (SMTC thumbnail, `Smtc::spotify_thumbnail`, decoded by `image` on a separate thread, read again 0.5 s then 2 s after each change); notification area icon (`tray-icon`, menu Show / Start-Stop / Quit, red while recording; closing the window during a recording hides it instead of quitting). Settings panel tested by the user.

Done on the engine side:

- `settings`: `Settings` as TOML in `%APPDATA%\Resona\settings.toml` (atomic write, damaged file set aside, missing fields defaulted), `recorder_config()`.
- `naming`: sub-folders (`Folders::Artist`, `ArtistAlbum`, from the album artist and album found by the tags), prefix (`TrackNumber` "04", `OrderNumber` "001" within the session).
- `library`: index of the tracks already there (recursive scan at start, key `Artist - Title` without prefix or "(2)"). `ExistingTracks::Skip` (not written at all, and optionally Spotify moves to the next one through SMTC), `Replace`, `KeepBoth`.
- Ads: Spotify muted in the mixer as soon as an ad is spotted, unmuted afterwards. **To check with a Free account**: if muting also reaches the capture, the start of the track after the ad could be lost (the sound is restored when the change is noticed, the cut is placed earlier).

Initial plan:

Port `frmEspionSpotify`: record button, log console, settings (folder, format, bitrate, Spotify quality, FLAC depth, minimum duration, ad options, language), minimize to the notification area.

- i18n: `@tr()` + gettext, `fr` and `en`. Replaces the `.resx`, `TranslationKeys` and `I18NKeys`. **English by default** (October 5, 2026): source texts are in English, French is a translation (`translations/fr/LC_MESSAGES/resona.po`). Errors go through `problem::Problem` (a kind, plus the untranslated technical cause): the interface words them in its language (`problem-text`); only the technical cause (Windows, Spotify) stays as is, in parentheses.
- Settings: a TOML file in `%APPDATA%\Resona`.

### Phase 5 bis — Playlist mode (October 4, 2026)

Paste a playlist, album or track link; the app records the whole list and stops on its own.

- **Driven by `spotify_cli.exe`**, the command-line tool Spotify ships with its client (Microsoft Store build 1.3.1.234, alias in `%LOCALAPPDATA%\Microsoft\WindowsApps`). JSON answers, 0.3 s per call. Tested: `playlist get` reads **private** playlists and whole albums (150 tracks at once), `play <uri>` starts the playlist from the beginning, `shuffle off`, `repeat off`, `now-playing`, `lookup --fields duration`. To check: whether the classic spotify.com installer ships it.
- Ruled out: the public embed page (`open.spotify.com/embed/…`, 100 tracks at most, 404 on private playlists); the `:play` suffix (does not start playback); UI automation (Spotify does not expose its buttons); injection through Chromium's debugging port (what Sidify does, fragile and against Spotify's terms).
- `spotify::link` (web or `spotify:` links), `spotify::cli` (windowless calls, timeout, JSON parsing tested on real answers).
- `recorder::playlist` (pure, tested): `Plan` says, from `now-playing`, what to do (wait, start the next track, finish). End: a track outside the list (autoplay) or a jump back to the start. `entry_for` ties a recording to its entry by title.
- `recorder::playlist_session`: shuffle and repeat off, Spotify's volume at 100 %, recorder started before playback, polling every second, Spotify paused at the end, report of the missed ones. "Retry the missed" starts them one by one (`Tracks` mode).
- Interface: link field, list shown in advance (○ waiting), header "name · done / total", link "Retry the N missed". `record_playlist <link>` example.
- Measured on "BEST JP" (private, 5 16-bit tracks, through the cable): 5 / 5 recorded, pause when autoplay arrived. 阿修羅ちゃん bit-perfect 16-bit; ダンスホール and Hi-Five peaks limited; Flow and ヨワネハキ slightly changed (close to the 24-bit grid) although they are 16-bit: Spotify's own processing, the same session having produced a bit-perfect one. Music video avoided (audio playback by default). The label "near lossless, 24-bit" becomes "slightly changed by Spotify".
- October 14, 2026 (Spotify 1.304.258, `spotify_cli` 1.3.4.258): answers unchanged. The volume set before `play` is not always kept: the session checks it again on each new track (`devices list`) and sets it back to 100 %.
- **Liked Songs**: `playlist get spotify:user:<name>:collection` returns all of them (1,284 in 0.7 s), `play` on the same URI starts them; `spotify:collection:tracks` fails for both ("client connection failed"), only `lookup` accepts it. The name comes from `spotify_cli me`. Links accepted: `open.spotify.com/collection/tracks`, `spotify:collection:tracks`, `spotify:user:<name>:collection`.
- Clipboard: a copied Spotify link fills the field (polled every second, only when the copied text changes, never during a recording). Read **only while a Resona window is in the foreground** (`GetForegroundWindow`): never in the background, as the clipboard may hold passwords.

### What Sidify taught us (October 14, 2026)

Analysis of Sidify All-In-One 2.6.4 (reverse engineering, `E:\Workspace\sidify\rapport_sidify_recorder_mode.md`).

- **"Convert" mode**: reimplements the Widevine licence exchange with an embedded Spotify device certificate, then decrypts the stream. Circumvention of a technical protection measure (DMCA, directive 2001/29/EC art. 6): **ruled out for good**.
- **"Record from App"**: a Detours DLL injected into Spotify's audio process, hooking `IAudioRenderClient::GetBuffer`/`ReleaseBuffer`. "High speed" makes it swallow buffers N times faster, then resamples. Ruled out: injection, Spotify's terms, and resampling destroys bit-perfect output.
- **"Record from Web"**: redirects the app's output to a virtual device (`Windows.Media.Internal.AudioPolicyConfig`), like our cable, but captures 16-bit and resamples: Resona keeps 32-bit float at 44.1 kHz.
- Track following: `r.js` reads the web player's internal React state every second (`__reactFiber$…` → `playerAPIInstance._state`), fragile with every update; `spotify_cli now-playing` is official.
- Taken over: watching the clipboard (`start-spotify-listen-clipboard`) and recording Liked Songs (`get-spotify-library-tracks`).
- Hint not followed for now: `media_handle.exe` has `record_count_down` / `noplay_count_down`; Sidify would start each track on its own and record it for its known duration. Our `Tracks` mode already does that for "Retry the missed"; make it the default if following the playlist (`Mode::Context`) disappoints.
- To do: podcasts (`show`, `episode`), which Sidify accepts and `LinkKind` still refuses.

### Spotify's settings and exact tags (October 4, 2026)

- `spotify::prefs` reads the `prefs` file of the last account used (Store build and classic installer): `audio.normalize_v2`, `audio.automix` (missing = on by default), `audio.play_bitrate_enumeration` (5 = Lossless). Warnings at launch and at the start of each recording. Crossfade is stored on Spotify's side (`audio.crossfade_migrated_to_ps`): only detection after the fact sees it.
- `metadata::spotify`: tags taken from Spotify's catalogue through `spotify_cli lookup`, for the version actually played. The engine follows `now-playing` every 2 s (`PlayingWatch`) and gives each recording the exact id; search is only a fallback (it does not always find the version played: 阿修羅ちゃん from the album 狂言 missing from the results). Album under SMTC's name (the CLI romanizes: "Kyougen"), album artists, date, copyright (`COPYRIGHT`, `TCOP`), 640 px cover art, position on the album (by id, title, then unique duration). Deezer adds genres, ISRC and label. `TagsOutcome { spotify, deezer }`.

### Lyrics (October 5, 2026)

- `metadata::lyrics`: lyrics from LRCLIB (lrclib.net, open database, no key), exact search (artist, title, album, duration) then free search filtered on title and duration (±3 s), synced preferred. Written as `.lrc` next to the file (same name), best effort; deleted with the file in "replace" mode. Setting "Save lyrics next to each track (.lrc)", checked by default; plain lyrics when LRCLIB has no timing; `RecorderEvent::Saved` says whether they are synced, plain, not found or failed (`LyricsOutcome`) (Files tab).
- Measured: synced lyrics for yes, and?, Flow and 阿修羅ちゃん (those two through the free search).
- Ruled out: Spotify's lyrics (Musixmatch, private API) and Canvas (private API, protected content).

### Phase 6 — Distribution

Done October 5, 2026, along with the rename to **Resona**:

- Icon (`crates/app/assets`): green disc and waves; built into the executable (`winresource` in `build.rs`), the window and the notification area (red while recording).
- `core::update`: latest GitHub release of `InstaZDLL/resona` (drafts and pre-releases ignored, versions compared numerically); "Resona X is available" warning with a link, setting "Check for updates at launch". Replaces `EspionSpotify.Updater`: the installer does the update.
- Installer: `cargo packager --release -p resona` (cargo-packager 0.11, NSIS, per-user install without administrator rights, French and English) → `target/packager/resona_<version>_x64-setup.exe` (8.5 MB).
- CI: `.github/workflows/ci.yml` (fmt, clippy `-D warnings`, tests on `windows-latest`) and `release.yml` (on a `v*` tag: tag version = `Cargo.toml` version, tests, installer, GitHub release).
- Done October 5, 2026: private repository `InstaZDLL/resona`, MIT licence, Resona logo and colours. Dependabot (`.github/dependabot.yml`: cargo and GitHub Actions every Monday, `windows`, `windows-core` and `wasapi` held to patch releases, as in WaveFlow). CodeQL (`codeql.yml`: Rust on Windows and workflows, `security-extended`, on every push, pull request, weekly, and before each release); the repository being private, the upload to the Security tab is attempted without blocking, and the job fails on its own if it finds anything. The release (`release.yml`) waits for the tag check and CodeQL. README redone with badges and screenshots (`RESONA_DEMO` demo mode).
- Authenticode signing (October 5, 2026), with WaveFlow's certificate (`WaveFlow/secrets/cert.pfx`, self-signed): secrets `SIGNTOOL_PFX_BASE64` and `SIGNTOOL_PFX_PASSWORD`. `release.yml` imports the certificate into the runner's store and gives its thumbprint to cargo-packager (its "command" mode splits on spaces, and signtool's path has some), which signs `resona.exe`, the uninstaller and the installer, timestamped by DigiCert; a step then checks thumbprint and timestamp. Tested October 5 with a throwaway certificate. Self-signed: status `UnknownError`, SmartScreen still warns; a certificate from a Trusted Root Program authority, put in the two secrets, will be enough.
- Licences (October 5, 2026, before making the repository public): `THIRD_PARTY_NOTICES.md` (Spytify lineage with its MIT notice, files taken from WaveFlow, LAME under LGPL statically linked, Slint under its Royalty-free 2.0 licence with its text) and `THIRD_PARTY_LICENSES.md` (generated by cargo-about 0.9.2 from `Cargo.lock`, Windows target: 323 entries). Both, plus `LICENSE.txt`, are installed next to `resona.exe`. "Made with Slint" badge in the README and the release notes (attribution required by Slint's licence). CI fails if the generated file is out of date; the release regenerates it before the installer. v0.1.0, built before, has these files attached to its release.
- Left: check `spotify_cli` with the classic installer.

## Lossless (new feature)

Spotify Lossless: Premium, *Audio quality → Lossless* setting in the client, FLAC up to 24-bit / 44.1 kHz. Some tracks are 16-bit masters.

What Resona does:

1. It captures at 44.1 kHz in 32-bit float (`format::CAPTURE_SAMPLE_RATE`), with no resampling on our side.
2. `analysis::BitAnalysis` checks every sample: if the signal was not changed, each float value is exactly a 16- or 24-bit integer. A single gain or resampling stage pushes almost every sample off this grid. A non-transparent capture is therefore detected over a whole track.
3. It writes FLAC with `BitDepth::Auto`: 16-bit if the source is, 24-bit otherwise.
4. It reports in the console, track by track, whether the capture was bit-transparent. If not, it gives the settings to check.

Settings asked of the user, from phase 0:

- Spotify: **Lossless** quality, volume slider at maximum, *Normalize volume* and **equalizer** off, **crossfade and Automix off** (otherwise track ends blend, whatever the format).
- Windows: **"Audio enhancements" Off** for the output device (verified cause), **device format at 44.1 kHz** (verified: 48 kHz resamples), spatial sound off, Spotify session at 100 % in the mixer.
- No need to touch the Windows volume or to switch a 7.1 headset back to stereo: the capture does not depend on them.

For the app itself to check (phase 5):

- `BitAnalysis` on each track, with the `capture_spotify` diagnosis (`estimate_gain` for a volume, `grid_residual` for an effect) turned into advice.
- Read the state of audio enhancements before recording (the device's `PKEY_AudioEndpoint_Disable_SysFx` property) and warn the user.
- Idea to study: if only a constant gain is in the way (Spotify slider not at maximum), dividing by the estimated step gives back the original integers exactly. To check block by block, as the gain can change within a track.

Interface choice: the Spotify quality (`format::SpotifyQuality`) is a setting. *Lossless* defaults to FLAC Auto, *Premium* to MP3 320, *Free* to MP3 160. Ads only exist in Free: the ad logic does not apply to the other two.

## Spotify Web API

The C# version (`API/SpotifyAPI.cs`) uses it for two things:

- **Now playing** (`GET /me/player`): exact track (ID), position, and `currently_playing_type = "ad"` to recognize ads.
- **Tags**: title, artists, track and disc number, album, album artists, release date, 640 px cover art. Album genres are almost always empty.

Since the February 2026 changes, for apps in development mode:

- the app's owner must have **Premium**, otherwise the app stops working. Each user creates their own app: Free users, the ones with ads, can no longer use it at all;
- `external_ids` (the **ISRC**) was removed from tracks and albums, as were `label` and `popularity`;
- batch retrieval (`GET /tracks`, `GET /albums`) was removed.

What is still useful compared with the other sources:

| Need | Without the Spotify API |
| --- | --- |
| Artist, title, album, position | SMTC (phase 1) |
| Track and disc number, date, HD cover art, genres, label, ISRC | Deezer, artist + title + album search (`album_match.rs`) |
| Recognize ads | Window title / SMTC (Free only) |
| Right version of the track (remaster, live, edition) | The Spotify ID is unambiguous; Deezer can pick the wrong version. It is the only real gain. |

Proposed decision: do not port it in the first version. If version mistakes show up in practice, add it as an option for Premium users (local OAuth PKCE, no client secret).

## C# → Rust mapping

| C# (`spy-spotify`) | Rust (`resona`) |
| --- | --- |
| `Spotify/SpotifyProcess.cs`, `SpotifyStatus.cs`, `SpotifyHandler.cs` | `core::spotify` (phase 1) |
| `AudioSessions/*`, `Router/*`, `Drivers/*` | `core::capture` (process loopback), `core::routing` (virtual cable, driver not bundled) |
| `Watcher.cs`, `Recorder.cs` | `core::recorder` (phase 2) |
| `Native/FileManager.cs` | `core::files` (phase 2) |
| `Recorder.GetMediaFileWriter` (NAudio.Lame) | `core::encode` |
| `API/*`, `MapperID3.cs` | `core::metadata`, `core::tags` (phase 4) |
| `frmEspionSpotify.cs`, `Translations/*` | `app` + `ui/*.slint` (phase 5) |
| `Settings.settings`, `Models/UserSettings.cs` | `core::settings` (TOML) |
| `EspionSpotify.Updater` | phase 6 |
