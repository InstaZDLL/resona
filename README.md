# Resona

Enregistreur Spotify pour Windows, réécriture en Rust de Spytify avec une interface Slint. Il capture uniquement le son du client Spotify, découpe les morceaux et les enregistre en MP3, WAV ou FLAC, y compris en **Lossless** (FLAC jusqu'à 24 bits / 44,1 kHz) pour les abonnés Premium.

Projet en cours de réécriture : voir [`docs/PLAN.md`](docs/PLAN.md).

```powershell
cargo run -p resona                                                     # application
cargo run -p resona-core --example capture_spotify -- 30 capture.flac  # test de capture
```
