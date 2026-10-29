use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use skratch::queue::Queue;
use std::{fs, io::Write, path::Path};

/// What the right-hand side of the Now playing page shows.
#[derive(Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum NowPanel {
    #[default]
    Details,
    Lyrics,
    UpNext,
    Library,
}

impl NowPanel {
    pub const ALL: [Self; 4] = [Self::Details, Self::Lyrics, Self::UpNext, Self::Library];

    pub fn title(self) -> &'static str {
        match self {
            Self::Details => "Details",
            Self::Lyrics => "Lyrics",
            Self::UpNext => "Up next",
            Self::Library => "Library",
        }
    }
}

/// Which lines get their rhymes coloured.
#[derive(Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum RhymeShow {
    Off,
    /// Only the line being sung.
    Current,
    #[default]
    All,
}

/// How the lyrics panel looks; every part can be switched off.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LyricStyle {
    /// Lines shrink and fade the further they are from the one being sung.
    pub fade: bool,
    /// Ad-libs in (brackets) are drawn smaller and dimmer.
    pub ad_libs: bool,
    /// Draw the `{effect|words}` formatting written in the lyrics.
    pub effects: bool,
    /// ...but only on the line being sung, like a lyric video.
    pub effects_live_only: bool,
    /// Let effects move (rainbow, wave, shake); off holds them still.
    pub animate: bool,
    pub rhymes: RhymeShow,
}

impl Default for LyricStyle {
    fn default() -> Self {
        Self {
            fade: true,
            ad_libs: true,
            effects: true,
            effects_live_only: true,
            animate: true,
            rhymes: RhymeShow::All,
        }
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Playlist {
    pub name: String,
    pub ids: Vec<i64>,
}

/// Where the queue came from, so searching the library while playing keeps to it.
#[derive(Clone, Default, Serialize, Deserialize)]
pub enum Source {
    /// Every track, in the library's sort order.
    #[default]
    Library,
    /// An album, artist or playlist: just these tracks, in this order.
    Tracks(Vec<i64>),
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedState {
    pub source: Source,
    /// Equalizer gains in dB for the ten bands in `player::EQ_BANDS`.
    pub eq: [f32; 10],
    pub playlists: Vec<Playlist>,
    pub queue: Queue,
    pub position: f64,
    pub volume: f64,
    pub download_source: Option<std::path::PathBuf>,
    pub download_format: skratch::download::DownloadFormat,
    /// Tracks start on a single click unless this is set.
    pub double_click: bool,
    /// Filename pattern for "Fix tags from filenames"; blank means the default.
    pub tag_pattern: String,
    /// "Also save into the audio file" in the online cover search; off unless chosen.
    pub embed_cover: bool,
    /// The first-run setup page has been shown (and dismissed) once.
    pub setup_done: bool,
    pub now_panel: NowPanel,
    /// Missing lyrics may be looked up on lrclib.net; off until the user allows it.
    pub lyrics_online: bool,
    pub lyric_style: LyricStyle,
}

impl SavedState {
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self {
                volume: 70.0,
                ..Self::default()
            });
        }
        let bytes = fs::read(path).context("Read player state")?;
        let mut state: Self = serde_json::from_slice(&bytes).context("Read saved player state")?;
        if !state.position.is_finite() || state.position < 0.0 {
            state.position = 0.0;
        }
        if !state.volume.is_finite() {
            state.volume = 70.0;
        }
        state.volume = state.volume.clamp(0.0, 100.0);
        Ok(state)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let mut temp = tempfile::NamedTempFile::new_in(path.parent().context("State directory")?)?;
        serde_json::to_writer(&mut temp, self)?;
        temp.flush()?;
        temp.as_file().sync_all()?;
        temp.persist(path).context("Save player state")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queue_and_position_roundtrip_and_invalid_file_is_reported() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("player.json");
        let mut state = SavedState::load(&path).unwrap();
        assert_eq!(state.volume, 70.0);
        state.queue.start(vec![1, 2, 3], 1);
        state.position = 42.5;
        state.save(&path).unwrap();
        let loaded = SavedState::load(&path).unwrap();
        assert_eq!(loaded.queue.current_id(), Some(2));
        assert_eq!(loaded.position, 42.5);
        fs::write(&path, b"invalid json").unwrap();
        assert!(SavedState::load(&path).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"invalid json");
    }
}
