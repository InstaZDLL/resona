//! What gets written into a recorded file, and writing it.

use std::path::Path;

use lofty::config::WriteOptions;
use lofty::ogg::OggPictureStorage;
use lofty::ogg::tag::VorbisComments;
use lofty::picture::{MimeType, Picture, PictureType};
use lofty::tag::{Accessor, ItemKey, ItemValue, Tag, TagExt, TagItem, TagType};

use super::deezer;
use crate::format::OutputFormat;
use crate::spotify::state::Track;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrackTags {
    pub title: String,
    /// Every artist, as Spotify lists them (`A, B`).
    pub artist: String,
    pub album: Option<String>,
    pub album_artist: Option<String>,
    pub track_number: Option<u32>,
    pub track_total: Option<u32>,
    pub disc_number: Option<u32>,
    /// `YYYY-MM-DD` or `YYYY`.
    pub date: Option<String>,
    pub genres: Vec<String>,
    pub isrc: Option<String>,
    pub label: Option<String>,
    pub cover: Option<Cover>,
    /// The catalogue the details come from, when not only Spotify.
    pub source: Option<&'static str>,
}

#[derive(Clone, PartialEq, Eq)]
pub struct Cover {
    pub data: Vec<u8>,
    pub mime: MimeType,
}

impl std::fmt::Debug for Cover {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Cover({:?}, {} bytes)", self.mime, self.data.len())
    }
}

impl Cover {
    /// Recognizes JPEG and PNG by their signature; other formats are not
    /// worth embedding.
    pub fn from_bytes(data: Vec<u8>) -> Option<Self> {
        let mime = if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
            MimeType::Jpeg
        } else if data.starts_with(b"\x89PNG\r\n\x1a\n") {
            MimeType::Png
        } else {
            return None;
        };
        Some(Self { data, mime })
    }
}

impl TrackTags {
    /// What Spotify itself says: the window title, plus SMTC's album
    /// details when they settled.
    pub fn from_spotify(track: &Track) -> Self {
        Self {
            title: track.title.full_title(),
            artist: track.title.artist.clone(),
            album: track.details.album.clone(),
            album_artist: track.details.album_artist.clone(),
            track_number: track.details.track_number,
            ..Self::default()
        }
    }

    /// Fills in what Deezer knows. Title and artist stay Spotify's (they
    /// name the file and list every artist); for the rest Deezer's
    /// catalogue is the more complete source.
    pub fn merge_deezer(&mut self, track: &deezer::Track, album: Option<&deezer::Album>) {
        let deezer_album = track.album.as_ref();
        if let Some(title) = album.map(|a| &a.title).or(deezer_album.map(|a| &a.title)) {
            self.album = Some(title.clone());
        }
        if let Some(artist) = album.and_then(|a| a.artist.as_ref()) {
            self.album_artist = Some(artist.name.clone());
        }
        self.track_number = track.track_position.or(self.track_number);
        self.disc_number = track.disk_number.or(self.disc_number);
        self.track_total = album.and_then(|a| a.nb_tracks).or(self.track_total);
        self.date = track
            .release_date
            .clone()
            .or_else(|| album.and_then(|a| a.release_date.clone()))
            .filter(|d| !d.starts_with("0000"));
        if let Some(album) = album {
            if let Some(genres) = &album.genres {
                self.genres = genres.data.iter().map(|g| g.name.clone()).collect();
            }
            self.label = album.label.clone().filter(|l| !l.is_empty());
        }
        self.isrc = track.isrc.clone().filter(|i| !i.is_empty());
        self.source = Some("Deezer");
    }

    /// The largest cover Deezer offers for this track's album.
    pub fn cover_url(track: &deezer::Track, album: Option<&deezer::Album>) -> Option<String> {
        album
            .and_then(|a| a.cover_xl.clone().or(a.cover_big.clone()))
            .or_else(|| {
                track
                    .album
                    .as_ref()
                    .and_then(|a| a.cover_xl.clone().or(a.cover_big.clone()))
            })
    }

    pub fn to_vorbis_comments(&self) -> VorbisComments {
        let mut tag = VorbisComments::default();
        tag.set_title(self.title.clone());
        tag.set_artist(self.artist.clone());
        if let Some(album) = &self.album {
            tag.set_album(album.clone());
        }
        let mut put = |key: &str, value: &Option<String>| {
            if let Some(value) = value {
                tag.insert(key.to_owned(), value.clone());
            }
        };
        put("ALBUMARTIST", &self.album_artist);
        put("DATE", &self.date);
        put("ISRC", &self.isrc);
        put("ORGANIZATION", &self.label);
        put("TRACKNUMBER", &self.track_number.map(|n| n.to_string()));
        put("TRACKTOTAL", &self.track_total.map(|n| n.to_string()));
        put("DISCNUMBER", &self.disc_number.map(|n| n.to_string()));
        for genre in &self.genres {
            tag.push("GENRE".to_owned(), genre.clone());
        }
        tag.insert(
            "ENCODER".to_owned(),
            concat!("Spytify ", env!("CARGO_PKG_VERSION")).to_owned(),
        );
        if let Some(cover) = &self.cover {
            let picture = Picture::unchecked(cover.data.clone())
                .pic_type(PictureType::CoverFront)
                .mime_type(cover.mime.clone())
                .build();
            // Reads the image header for the FLAC picture block; a
            // corrupt image is left out rather than failing the tags.
            if tag.insert_picture(picture, None).is_err() {
                tracing::warn!("cover image unreadable, not embedded");
            }
        }
        tag
    }

    /// Writes the tags into a FLAC file, through the concrete Vorbis
    /// comment tag (a generic `Tag` would drop non-standard fields).
    pub fn write_flac(&self, path: &Path) -> crate::Result<()> {
        self.to_vorbis_comments()
            .save_to_path(path, WriteOptions::default())
            .map_err(|e| crate::Error::Tags(e.to_string()))
    }

    /// Tags the file in the format it was encoded to: Vorbis comments for
    /// FLAC, ID3v2 for MP3 and WAV.
    pub fn write(&self, path: &Path, format: OutputFormat) -> crate::Result<()> {
        match format {
            OutputFormat::Flac { .. } => self.write_flac(path),
            OutputFormat::Mp3 { .. } | OutputFormat::Wav { .. } => self.write_id3v2(path),
        }
    }

    /// ID3v2 through lofty's generic tag: the file was just encoded and
    /// holds no other tag, so there are no foreign fields to preserve.
    pub fn to_id3v2(&self) -> Tag {
        let mut tag = Tag::new(TagType::Id3v2);
        tag.set_title(self.title.clone());
        tag.set_artist(self.artist.clone());
        if let Some(album) = &self.album {
            tag.set_album(album.clone());
        }
        let mut put = |key: ItemKey, value: Option<String>| {
            if let Some(value) = value {
                tag.insert_text(key, value);
            }
        };
        put(ItemKey::AlbumArtist, self.album_artist.clone());
        put(ItemKey::RecordingDate, self.date.clone());
        put(ItemKey::Isrc, self.isrc.clone());
        put(ItemKey::Label, self.label.clone());
        put(
            ItemKey::TrackNumber,
            self.track_number.map(|n| n.to_string()),
        );
        put(ItemKey::TrackTotal, self.track_total.map(|n| n.to_string()));
        put(ItemKey::DiscNumber, self.disc_number.map(|n| n.to_string()));
        put(
            ItemKey::EncoderSoftware,
            Some(concat!("Spytify ", env!("CARGO_PKG_VERSION")).to_owned()),
        );
        for genre in &self.genres {
            tag.push(TagItem::new(ItemKey::Genre, ItemValue::Text(genre.clone())));
        }
        if let Some(cover) = &self.cover {
            tag.push_picture(
                Picture::unchecked(cover.data.clone())
                    .pic_type(PictureType::CoverFront)
                    .mime_type(cover.mime.clone())
                    .build(),
            );
        }
        tag
    }

    pub fn write_id3v2(&self, path: &Path) -> crate::Result<()> {
        self.to_id3v2()
            .save_to_path(path, WriteOptions::default())
            .map_err(|e| crate::Error::Tags(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spotify::state::TrackDetails;
    use crate::spotify::title::{TitleSeparator, TrackTitle};

    fn spotify_track() -> Track {
        Track {
            title: TrackTitle {
                artist: "Daft Punk".into(),
                title: "Harder, Better, Faster, Stronger".into(),
                title_extended: None,
                separator: TitleSeparator::None,
            },
            details: TrackDetails {
                album: Some("Discovery".into()),
                album_artist: Some("Daft Punk".into()),
                track_number: Some(4),
                duration: None,
            },
        }
    }

    #[test]
    fn merges_deezer_over_spotify() {
        let mut tags = TrackTags::from_spotify(&spotify_track());
        let track: deezer::Track = serde_json::from_str(
            r#"{"id":1,"title":"x","isrc":"GBDUW0000059","track_position":4,"disk_number":1,
                "release_date":"2001-03-07","album":{"id":2,"title":"Discovery","cover_xl":"u"}}"#,
        )
        .unwrap();
        let album: deezer::Album = serde_json::from_str(
            r#"{"id":2,"title":"Discovery","label":"Parlophone","nb_tracks":14,
                "genres":{"data":[{"name":"Electro"},{"name":"Dance"}]},"artist":{"name":"Daft Punk"}}"#,
        )
        .unwrap();
        tags.merge_deezer(&track, Some(&album));
        assert_eq!(tags.title, "Harder, Better, Faster, Stronger");
        assert_eq!(
            (tags.track_number, tags.track_total, tags.disc_number),
            (Some(4), Some(14), Some(1))
        );
        assert_eq!(tags.date.as_deref(), Some("2001-03-07"));
        assert_eq!(tags.genres, ["Electro", "Dance"]);
        assert_eq!(tags.label.as_deref(), Some("Parlophone"));
        assert_eq!(
            TrackTags::cover_url(&track, Some(&album)).as_deref(),
            Some("u")
        );
    }

    #[test]
    fn writes_and_reads_back_a_flac() {
        use crate::encode::{flac::encode_wav_to_flac, wav::CaptureWav};
        use lofty::file::TaggedFileExt;
        use lofty::tag::ItemKey;

        let dir = std::env::temp_dir().join(format!("spytify-tags-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (wav, flac) = (dir.join("t.wav"), dir.join("t.flac"));
        let mut writer = CaptureWav::create(&wav).unwrap();
        writer.write(&[0.25; 8_820]).unwrap();
        writer.finalize().unwrap();
        encode_wav_to_flac(&wav, &flac, crate::encode::Quantizer::new(16, 16)).unwrap();

        let mut tags = TrackTags::from_spotify(&spotify_track());
        tags.genres = vec!["Electro".into()];
        // A 1×1 PNG.
        tags.cover = Cover::from_bytes(
            b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\x01\0\0\0\x01\x08\x06\0\0\0\x1f\x15\xc4\x89\0\0\0\rIDATx\x9cc\xf8\xff\xff?\0\x05\xfe\x02\xfe\xa7\x35\x81\x84\0\0\0\0IEND\xaeB`\x82"
                .to_vec(),
        );
        tags.write_flac(&flac).unwrap();

        let tagged = lofty::read_from_path(&flac).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        let tag = tagged.primary_tag().unwrap();
        assert_eq!(
            tag.title().as_deref(),
            Some("Harder, Better, Faster, Stronger")
        );
        assert_eq!(tag.get_string(ItemKey::AlbumArtist), Some("Daft Punk"));
        assert_eq!(tag.track(), Some(4));
        assert_eq!(tag.pictures().len(), 1);
    }

    #[test]
    fn writes_and_reads_back_an_mp3() {
        use crate::encode::{mp3::encode_wav_to_mp3, wav::CaptureWav};
        use lofty::file::TaggedFileExt;
        use lofty::tag::ItemKey;

        let dir = std::env::temp_dir().join(format!("spytify-id3-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (wav, mp3) = (dir.join("t.wav"), dir.join("t.mp3"));
        let mut writer = CaptureWav::create(&wav).unwrap();
        writer.write(&[0.25; 88_200]).unwrap();
        writer.finalize().unwrap();
        encode_wav_to_mp3(&wav, &mp3, 192).unwrap();

        let mut tags = TrackTags::from_spotify(&spotify_track());
        tags.genres = vec!["Electro".into(), "Dance".into()];
        tags.isrc = Some("GBDUW0000059".into());
        tags.cover = Cover::from_bytes(vec![0xFF, 0xD8, 0xFF, 0xE0, 0, 0x10]);
        tags.write(&mp3, OutputFormat::Mp3 { kbps: 192 }).unwrap();

        let tagged = lofty::read_from_path(&mp3).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        let tag = tagged.primary_tag().unwrap();
        assert_eq!(tag.tag_type(), TagType::Id3v2);
        assert_eq!(
            tag.title().as_deref(),
            Some("Harder, Better, Faster, Stronger")
        );
        assert_eq!(tag.get_string(ItemKey::AlbumArtist), Some("Daft Punk"));
        assert_eq!(tag.get_string(ItemKey::Isrc), Some("GBDUW0000059"));
        assert_eq!(tag.track(), Some(4));
        assert_eq!(tag.get_strings(ItemKey::Genre).count(), 2);
        assert_eq!(tag.pictures().len(), 1);
    }
}
