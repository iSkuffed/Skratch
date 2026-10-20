use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use skratch::queue::Queue;
use std::{fs, io::Write, path::Path};

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedState {
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
