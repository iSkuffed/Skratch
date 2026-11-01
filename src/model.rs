use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Track {
    pub id: i64,
    pub path: PathBuf,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub album_artist: String,
    pub genre: String,
    pub year: Option<u32>,
    pub track_number: Option<u32>,
    pub disc_number: Option<u32>,
    pub duration: f64,
    pub artwork: Option<PathBuf>,
    /// Times it has been played through (kept in the library database, not the file).
    #[serde(default)]
    pub plays: u32,
    /// When it was last played, in seconds since the Unix epoch.
    #[serde(default)]
    pub last_played: Option<i64>,
    /// Stars from 1 to 5; 0 means not rated.
    #[serde(default)]
    pub rating: u8,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct MetadataEdit {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub album_artist: String,
    pub genre: String,
    pub year: Option<u32>,
    pub track_number: Option<u32>,
    pub disc_number: Option<u32>,
    /// A single filename in the existing directory, including the unchanged extension.
    pub filename: String,
}

impl From<&Track> for MetadataEdit {
    fn from(t: &Track) -> Self {
        Self {
            title: t.title.clone(),
            artist: t.artist.clone(),
            album: t.album.clone(),
            album_artist: t.album_artist.clone(),
            genre: t.genre.clone(),
            year: t.year,
            track_number: t.track_number,
            disc_number: t.disc_number,
            filename: t
                .path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
        }
    }
}
