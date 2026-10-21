//! Lyrics for the Now playing panel. Looked up in order: a `.lrc` file next to the track,
//! lyrics in the file's own tags, a local cache, and (only when the user allows it) LRCLIB.
use crate::{model::Track, online, tagparse};
use eframe::egui;
use anyhow::Result;
use lofty::{prelude::*, tag::ItemKey};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, Sender},
    time::{Duration, SystemTime},
};

/// How long "LRCLIB has nothing" is believed before asking again.
const MISS_TTL: Duration = Duration::from_secs(7 * 24 * 3600);

#[derive(Debug, Clone, PartialEq)]
pub enum Lyrics {
    /// `(seconds, text)`, sorted by time. Blank text is a pause between verses.
    Synced(Vec<(f64, String)>),
    Plain(String),
}

impl Lyrics {
    /// Lines with `[mm:ss.xx]` stamps become `Synced`; anything else is kept as plain text.
    pub fn from_text(text: &str) -> Option<Self> {
        let synced = parse_lrc(text);
        if !synced.is_empty() {
            return Some(Self::Synced(synced));
        }
        let text = text.trim();
        (!text.is_empty()).then(|| Self::Plain(text.replace("\r\n", "\n")))
    }
}

/// Reads an LRC file. A line may carry several stamps; tags like `[ar:Artist]` are skipped
/// and word-by-word `<mm:ss.xx>` stamps are dropped.
pub fn parse_lrc(text: &str) -> Vec<(f64, String)> {
    let mut lines = Vec::new();
    for line in text.lines() {
        let mut rest = line.trim();
        let mut times = Vec::new();
        while let Some(tail) = rest.strip_prefix('[') {
            let Some(end) = tail.find(']') else { break };
            if let Some(time) = stamp(&tail[..end]) {
                times.push(time);
            }
            rest = tail[end + 1..].trim_start();
        }
        let mut words = String::new();
        let mut skipping = false;
        for c in rest.chars() {
            match c {
                '<' => skipping = true,
                '>' if skipping => skipping = false,
                _ if !skipping => words.push(c),
                _ => {}
            }
        }
        let words = words.trim();
        lines.extend(times.into_iter().map(|t| (t, words.to_string())));
    }
    lines.sort_by(|a, b| a.0.total_cmp(&b.0));
    lines
}

/// `mm:ss`, `mm:ss.xx` or `mm:ss:xx`; `None` for `ar:Artist` and the like.
fn stamp(text: &str) -> Option<f64> {
    let (minutes, seconds) = text.split_once(':')?;
    let minutes: f64 = minutes.trim().parse().ok()?;
    let seconds: f64 = seconds.trim().replacen(':', ".", 1).parse().ok()?;
    (minutes >= 0.0 && seconds >= 0.0).then_some(minutes * 60.0 + seconds)
}

/// The best LRCLIB search result for a track of `duration` seconds: it has to be about as
/// long as the file (the same song, not a live cut) and synced lyrics win over plain.
fn pick(results: &Value, duration: f64) -> Option<Lyrics> {
    let text = |item: &Value, key: &str| {
        item[key]
            .as_str()
            .filter(|t| !t.trim().is_empty())
            .and_then(Lyrics::from_text)
    };
    let close = |item: &&Value| {
        duration <= 0.0
            || item["duration"]
                .as_f64()
                .is_none_or(|d| (d - duration).abs() <= 5.0)
    };
    let items: Vec<&Value> = results.as_array()?.iter().filter(close).collect();
    items
        .iter()
        .find_map(|item| text(item, "syncedLyrics"))
        .or_else(|| items.iter().find_map(|item| text(item, "plainLyrics")))
}

/// LRCLIB answers 503 to quick bursts, so a failed request is repeated once after a pause.
fn search(query: &[(&str, &str)]) -> Result<Value> {
    const URL: &str = "https://lrclib.net/api/search";
    online::get_json(URL, query).or_else(|_| {
        std::thread::sleep(Duration::from_secs(2));
        online::get_json(URL, query)
    })
}

fn lrclib(track: &Track) -> Result<Option<Lyrics>> {
    let title = tagparse::strip_noise(&track.title);
    let artist = track.artist.trim();
    if !artist.is_empty() {
        let found = search(&[("track_name", &title), ("artist_name", artist)])?;
        if let Some(lyrics) = pick(&found, track.duration) {
            return Ok(Some(lyrics));
        }
    }
    let q = format!("{artist} {title}");
    let found = search(&[("q", q.trim())])?;
    Ok(pick(&found, track.duration))
}

fn embedded(path: &Path) -> Option<Lyrics> {
    let file = lofty::read_from_path(path).ok()?;
    let tag = file.primary_tag().or_else(|| file.first_tag())?;
    Lyrics::from_text(tag.get_string(&ItemKey::Lyrics)?)
}

/// Where a track's online result is remembered, independent of where the file lives.
fn cache_file(dir: &Path, track: &Track) -> PathBuf {
    let key = format!(
        "{}\0{}\0{}",
        track.artist.to_lowercase(),
        tagparse::strip_noise(&track.title).to_lowercase(),
        track.duration.round()
    );
    let hash = Sha256::digest(key.as_bytes());
    let name: String = hash.iter().take(12).map(|b| format!("{b:02x}")).collect();
    dir.join(format!("{name}.json"))
}

fn find(track: &Track, dir: &Path, online: bool) -> Result<Option<Lyrics>> {
    if let Some(lyrics) = fs::read_to_string(track.path.with_extension("lrc"))
        .ok()
        .and_then(|text| Lyrics::from_text(&text))
        .or_else(|| embedded(&track.path))
    {
        return Ok(Some(lyrics));
    }
    let cache = cache_file(dir, track);
    if let Ok(bytes) = fs::read(&cache)
        && let Ok(saved) = serde_json::from_slice::<Value>(&bytes)
    {
        let text = |key: &str| saved[key].as_str().and_then(Lyrics::from_text);
        if let Some(lyrics) = text("synced").or_else(|| text("plain")) {
            return Ok(Some(lyrics));
        }
        let fresh = fs::metadata(&cache)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| SystemTime::now().duration_since(t).ok())
            .is_some_and(|age| age < MISS_TTL);
        if fresh {
            return Ok(None);
        }
    }
    if !online {
        return Ok(None);
    }
    let lyrics = lrclib(track)?;
    let (synced, plain) = match &lyrics {
        Some(Lyrics::Synced(lines)) => (
            Some(
                lines
                    .iter()
                    .map(|(t, text)| {
                        format!("[{:02}:{:05.2}]{text}\n", (*t / 60.0) as u32, t % 60.0)
                    })
                    .collect::<String>(),
            ),
            None,
        ),
        Some(Lyrics::Plain(text)) => (None, Some(text.clone())),
        None => (None, None),
    };
    // a failed cache write only costs a repeat lookup next time
    let _ = fs::create_dir_all(dir)
        .and_then(|()| fs::write(&cache, json!({"synced": synced, "plain": plain}).to_string()));
    Ok(lyrics)
}

pub enum Status {
    Loading,
    Found(Lyrics),
    Missing,
    Failed(String),
}

/// Looks the playing track's lyrics up in the background and holds the answer.
pub struct Lookup {
    tx: Sender<(PathBuf, bool, Result<Option<Lyrics>>)>,
    rx: Receiver<(PathBuf, bool, Result<Option<Lyrics>>)>,
    track: Option<PathBuf>,
    /// Whether the answer in `status` already included the online lookup.
    asked_online: bool,
    pub status: Status,
    /// The synced line the panel last scrolled to.
    pub scrolled: Option<usize>,
}

impl Default for Lookup {
    fn default() -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            tx,
            rx,
            track: None,
            asked_online: false,
            status: Status::Loading,
            scrolled: None,
        }
    }
}

impl Lookup {
    /// Starts a lookup for `track` if it changed (or `online` was just allowed and the first
    /// answer was "nothing"), and collects a finished one into `status`.
    pub fn update(&mut self, ctx: &egui::Context, track: &Track, dir: &Path, online: bool) {
        let changed = self.track.as_deref() != Some(track.path.as_path());
        let allowed_now = online && !self.asked_online && matches!(self.status, Status::Missing);
        if changed || allowed_now {
            self.track = Some(track.path.clone());
            self.asked_online = online;
            self.status = Status::Loading;
            self.scrolled = None;
            self.spawn(ctx, track.clone(), dir.to_path_buf(), online);
        }
        while let Ok((path, _, result)) = self.rx.try_recv() {
            if self.track.as_deref() == Some(path.as_path()) {
                self.status = match result {
                    Ok(Some(lyrics)) => Status::Found(lyrics),
                    Ok(None) => Status::Missing,
                    Err(error) => Status::Failed(format!("{error:#}")),
                };
            }
        }
    }

    /// Looks again, e.g. after a network failure.
    pub fn retry(&mut self) {
        self.track = None;
    }

    fn spawn(&self, ctx: &egui::Context, track: Track, dir: PathBuf, online: bool) {
        let (tx, ctx) = (self.tx.clone(), ctx.clone());
        std::thread::spawn(move || {
            let result = find(&track, &dir, online);
            let _ = tx.send((track.path, online, result));
            ctx.request_repaint();
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lrc_text_is_parsed_into_timed_lines() {
        let lines = parse_lrc(
            "[ar:Someone]\n[00:12.50]first <00:13.00>line\n[01:05]second\n[00:01.00][00:30.00]\n",
        );
        assert_eq!(
            lines,
            vec![
                (1.0, String::new()),
                (12.5, "first line".into()),
                (30.0, String::new()),
                (65.0, "second".into()),
            ]
        );
        assert_eq!(
            Lyrics::from_text("just words\nno stamps"),
            Some(Lyrics::Plain("just words\nno stamps".into()))
        );
        assert_eq!(Lyrics::from_text("  \n"), None);
    }

    #[test]
    fn search_results_must_match_the_length_and_prefer_synced() {
        let results = json!([
            {"duration": 400.0, "syncedLyrics": "[00:01.00]live version"},
            {"duration": 181.0, "syncedLyrics": null, "plainLyrics": "plain"},
            {"duration": 180.0, "syncedLyrics": "[00:02.00]right one", "plainLyrics": "plain"},
        ]);
        assert_eq!(
            pick(&results, 180.4),
            Some(Lyrics::Synced(vec![(2.0, "right one".into())]))
        );
        assert_eq!(pick(&results, 30.0), None);
        assert_eq!(pick(&json!([]), 180.0), None);
    }

    #[test]
    fn a_sidecar_lrc_is_found_without_the_network() {
        let dir = tempfile::tempdir().unwrap();
        let song = dir.path().join("song.mp3");
        fs::write(&song, b"not really audio").unwrap();
        fs::write(dir.path().join("song.lrc"), "[00:05.00]hello").unwrap();
        let track = Track {
            path: song,
            ..Track::default()
        };
        assert_eq!(
            find(&track, &dir.path().join("cache"), false).unwrap(),
            Some(Lyrics::Synced(vec![(5.0, "hello".into())]))
        );
    }
}
