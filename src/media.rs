use crate::model::Track;
use anyhow::{Context, Result, bail};
use souvlaki::{
    MediaControlEvent, MediaControls, MediaMetadata, MediaPlayback, MediaPosition, PlatformConfig,
    SeekDirection,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;
use url::Url;

#[derive(Debug, Clone, PartialEq)]
pub enum MediaCommand {
    Play,
    Pause,
    Toggle,
    Next,
    Previous,
    Stop,
    SeekBy(f64),
    SeekTo(f64),
    SetVolume(f64),
    Raise,
    Quit,
}

pub struct DesktopControls {
    pub events: Receiver<MediaCommand>,
    controls: MediaControls,
    last_metadata: Option<MetadataSnapshot>,
    last_playback: Option<PlaybackSnapshot>,
    last_volume: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct MetadataSnapshot {
    path: Option<PathBuf>,
    title: Option<String>,
    artist: Option<String>,
    album: Option<String>,
    cover_url: Option<String>,
    duration: Option<Duration>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PlaybackSnapshot {
    path: Option<PathBuf>,
    playing: bool,
    position: Duration,
}

impl DesktopControls {
    pub fn new(wake: Arc<dyn Fn() + Send + Sync>) -> Result<Self> {
        let mut controls = MediaControls::new(PlatformConfig {
            dbus_name: "skratch",
            display_name: "Skratch",
            hwnd: None,
        })
        .context("initialize Linux desktop media controls")?;
        let (sender, events) = mpsc::channel();
        controls
            .attach(move |event| {
                if let Some(command) = map_event(event)
                    && sender.send(command).is_ok()
                {
                    wake();
                }
            })
            .context("attach Skratch to the desktop media controls")?;

        Ok(Self {
            events,
            controls,
            last_metadata: None,
            last_playback: None,
            last_volume: None,
        })
    }

    pub fn update(
        &mut self,
        track: Option<&Track>,
        playing: bool,
        position: f64,
        volume: f64,
    ) -> Result<()> {
        if !volume.is_finite() || !(0.0..=100.0).contains(&volume) {
            bail!("desktop media volume must be between 0 and 100");
        }
        let position = checked_duration(position).context("invalid desktop media position")?;
        let metadata = track.map(metadata_snapshot).unwrap_or_default();
        if self.last_metadata.as_ref() != Some(&metadata) {
            self.controls
                .set_metadata(MediaMetadata {
                    title: metadata.title.as_deref(),
                    artist: metadata.artist.as_deref(),
                    album: metadata.album.as_deref(),
                    cover_url: metadata.cover_url.as_deref(),
                    duration: metadata.duration,
                })
                .context("publish desktop media metadata")?;
            self.last_metadata = Some(metadata.clone());
        }

        let playback = PlaybackSnapshot {
            path: metadata.path.clone(),
            playing: track.is_some() && playing,
            position: if track.is_some() {
                position
            } else {
                Duration::ZERO
            },
        };
        let playback_changed = self.last_playback.as_ref().is_none_or(|last| {
            last.path != playback.path
                || last.playing != playback.playing
                || last.position.abs_diff(playback.position) >= Duration::from_secs(1)
        });
        if playback_changed {
            let progress = Some(MediaPosition(playback.position));
            let state = if track.is_none() {
                MediaPlayback::Stopped
            } else if playback.playing {
                MediaPlayback::Playing { progress }
            } else {
                MediaPlayback::Paused { progress }
            };
            self.controls
                .set_playback(state)
                .context("publish desktop media playback position")?;
            self.last_playback = Some(playback);
        }

        if self
            .last_volume
            .is_none_or(|last| (last - volume).abs() >= 0.001)
        {
            self.controls
                .set_volume(volume / 100.0)
                .context("publish desktop media volume")?;
            self.last_volume = Some(volume);
        }
        Ok(())
    }
}

fn metadata_snapshot(track: &Track) -> MetadataSnapshot {
    MetadataSnapshot {
        path: Some(track.path.clone()),
        title: nonempty(&track.title),
        artist: nonempty(&track.artist),
        album: nonempty(&track.album),
        cover_url: track.artwork.as_deref().and_then(file_url),
        duration: checked_duration(track.duration).ok(),
    }
}

fn nonempty(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_owned())
}

fn file_url(path: &Path) -> Option<String> {
    let path = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir().ok()?.join(path)
    };
    Url::from_file_path(path).ok().map(Into::into)
}

fn checked_duration(seconds: f64) -> Result<Duration> {
    if !seconds.is_finite() || seconds < 0.0 {
        bail!("seconds must be finite and non-negative");
    }
    let duration = Duration::try_from_secs_f64(seconds)
        .map_err(|_| anyhow::anyhow!("duration is out of range"))?;
    if duration.as_micros() > i64::MAX as u128 {
        bail!("duration exceeds the MPRIS position limit");
    }
    Ok(duration)
}

fn map_event(event: MediaControlEvent) -> Option<MediaCommand> {
    Some(match event {
        MediaControlEvent::Play => MediaCommand::Play,
        MediaControlEvent::Pause => MediaCommand::Pause,
        MediaControlEvent::Toggle => MediaCommand::Toggle,
        MediaControlEvent::Next => MediaCommand::Next,
        MediaControlEvent::Previous => MediaCommand::Previous,
        MediaControlEvent::Stop => MediaCommand::Stop,
        MediaControlEvent::Seek(direction) => MediaCommand::SeekBy(match direction {
            SeekDirection::Forward => 10.0,
            SeekDirection::Backward => -10.0,
        }),
        MediaControlEvent::SeekBy(direction, duration) => {
            let seconds = duration.as_secs_f64();
            MediaCommand::SeekBy(match direction {
                SeekDirection::Forward => seconds,
                SeekDirection::Backward => -seconds,
            })
        }
        MediaControlEvent::SetPosition(position) => MediaCommand::SeekTo(position.0.as_secs_f64()),
        MediaControlEvent::SetVolume(volume) if volume.is_finite() => {
            MediaCommand::SetVolume(volume.clamp(0.0, 1.0) * 100.0)
        }
        MediaControlEvent::SetVolume(_) => return None,
        MediaControlEvent::Raise => MediaCommand::Raise,
        MediaControlEvent::Quit => MediaCommand::Quit,
        MediaControlEvent::OpenUri(_) => return None,
    })
}
