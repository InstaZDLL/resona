//! Phase 4 harness: the tags Resona would write for a track.
//!
//! ```text
//! cargo run -p resona-core --example tag_lookup -- "Artist" "Title" ["Album"] [seconds] [--uri=spotify:track:…]
//! ```

use std::time::Duration;

use resona_core::metadata::deezer::DeezerClient;
use resona_core::metadata::tags_for;
use resona_core::spotify::state::{Track, TrackDetails};
use resona_core::spotify::title;

fn main() -> anyhow::Result<()> {
    let (flags, args): (Vec<String>, Vec<String>) =
        std::env::args().skip(1).partition(|a| a.starts_with("--"));
    let uri = flags.iter().find_map(|f| f.strip_prefix("--uri="));
    let [artist, title, rest @ ..] = args.as_slice() else {
        anyhow::bail!(
            "usage: tag_lookup <artist> <title> [album] [seconds] [--uri=spotify:track:…]"
        );
    };
    let title::TitleState::Track(parsed) = title::parse(&format!("{artist} - {title}")) else {
        anyhow::bail!("not an \"artist - title\" pair");
    };
    let track = Track {
        title: parsed,
        details: TrackDetails {
            album: rest.first().cloned(),
            album_artist: None,
            track_number: None,
            duration: rest
                .get(1)
                .map(|s| s.parse())
                .transpose()?
                .map(Duration::from_secs),
        },
    };
    let cli = resona_core::spotify::cli::SpotifyCli::find().ok();
    let (tags, outcome) = tags_for(Some(&DeezerClient::new()?), cli.as_ref(), &track, uri);
    println!("{outcome:?}\n{tags:#?}");
    Ok(())
}
