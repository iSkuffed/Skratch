//! Lyrics for the Now playing panel. Looked up in order: a `.lrc` file next to the track,
//! lyrics in the file's own tags, a local cache, and (only when the user allows it) LRCLIB,
//! then NetEase Cloud Music as a fallback when LRCLIB has nothing or is down.
use crate::{model::Track, online, tagparse};
use anyhow::Result;
use eframe::egui;
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

impl Lyrics {
    /// The text form the editor shows and `from_text` reads back.
    pub fn to_text(&self) -> String {
        match self {
            Self::Plain(text) => text.clone(),
            Self::Synced(lines) => lines
                .iter()
                .map(|(t, text)| format!("[{:02}:{:05.2}]{text}\n", (*t / 60.0) as u32, t % 60.0))
                .collect(),
        }
    }
}

/// Puts a `[mm:ss.xx]` stamp at the start of the line holding char index `cursor` (replacing
/// any it already has) and returns the new text with the cursor moved to the next line.
pub fn stamp_line(text: &str, cursor: usize, seconds: f64) -> (String, usize) {
    let chars: Vec<char> = text.chars().collect();
    let cursor = cursor.min(chars.len());
    let start = chars[..cursor]
        .iter()
        .rposition(|c| *c == '\n')
        .map_or(0, |i| i + 1);
    let end = chars[cursor..]
        .iter()
        .position(|c| *c == '\n')
        .map_or(chars.len(), |i| cursor + i);
    let line: String = chars[start..end].iter().collect();
    let mut rest = line.trim_start();
    while let Some(tail) = rest.strip_prefix('[') {
        match tail.find(']') {
            Some(close) if stamp(&tail[..close]).is_some() => rest = tail[close + 1..].trim_start(),
            _ => break,
        }
    }
    let stamped = format!(
        "[{:02}:{:05.2}]{rest}",
        (seconds / 60.0) as u32,
        seconds % 60.0
    );
    let before: String = chars[..start].iter().collect();
    let after: String = chars[end..].iter().collect();
    let next = before.chars().count() + stamped.chars().count() + 1;
    let after = if after.is_empty() {
        "\n".to_string()
    } else {
        after
    };
    (format!("{before}{stamped}{after}"), next)
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

/// NetEase song ids worth fetching lyrics for: about as long as the file and by the same
/// artist (when the track has one), best match first.
fn netease_ids(results: &Value, duration: f64, artist: &str) -> Vec<u64> {
    let artist = artist.trim().to_lowercase();
    let by_artist = |song: &Value| {
        artist.is_empty()
            || song["artists"].as_array().is_some_and(|names| {
                names.iter().filter_map(|a| a["name"].as_str()).any(|name| {
                    let name = name.trim().to_lowercase();
                    !name.is_empty() && (artist.contains(&name) || name.contains(&artist))
                })
            })
    };
    let close = |song: &Value| {
        duration <= 0.0
            || song["duration"]
                .as_f64()
                .is_some_and(|ms| (ms / 1000.0 - duration).abs() <= 5.0)
    };
    results["result"]["songs"]
        .as_array()
        .map(|songs| {
            songs
                .iter()
                .filter(|song| close(song) && by_artist(song))
                .filter_map(|song| song["id"].as_u64())
                .collect()
        })
        .unwrap_or_default()
}

/// NetEase puts credits (`作词 : ...`) in the first lines; they are not lyrics.
fn without_credits(lrc: &str) -> String {
    const CREDITS: [&str; 8] = [
        "作词", "作曲", "编曲", "制作", "混音", "母带", "录音", "监制",
    ];
    lrc.lines()
        .filter(|line| {
            let text = line.rsplit(']').next().unwrap_or(line).trim();
            !CREDITS.iter().any(|c| {
                text.strip_prefix(c)
                    .is_some_and(|rest| rest.trim_start().starts_with([':', '：']))
            })
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// NetEase Cloud Music's public search and lyric endpoints. Unofficial and liable to change,
/// so callers treat a failure here as "this source did not answer", never as fatal.
fn netease(track: &Track) -> Result<Option<Lyrics>> {
    let q = format!(
        "{} {}",
        track.artist.trim(),
        tagparse::strip_noise(&track.title)
    );
    let found = online::get_json(
        "https://music.163.com/api/search/get",
        &[("s", q.trim()), ("type", "1"), ("limit", "10")],
    )?;
    for id in netease_ids(&found, track.duration, &track.artist)
        .into_iter()
        .take(3)
    {
        let id = id.to_string();
        let answer = online::get_json(
            "https://music.163.com/api/song/lyric",
            &[("id", &id), ("lv", "1"), ("kv", "1"), ("tv", "-1")],
        )?;
        if let Some(lyrics) = answer["lrc"]["lyric"]
            .as_str()
            .and_then(|lrc| Lyrics::from_text(&without_credits(lrc)))
        {
            return Ok(Some(lyrics));
        }
    }
    Ok(None)
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

/// The user's own lyrics for a track (typed, imported, or empty for "none"). They win over
/// everything else and live in the app's data, so no music file is touched.
fn user_file(dir: &Path, track: &Track) -> PathBuf {
    cache_file(dir, track).with_extension("user.txt")
}

/// Keeps `text` as this track's lyrics; empty text means "no lyrics, stop looking".
pub fn save(dir: &Path, track: &Track, text: &str) -> std::io::Result<()> {
    fs::create_dir_all(dir)?;
    fs::write(user_file(dir, track), text)
}

/// Drops the user's version and the remembered online answer, so the next lookup starts over.
pub fn restore(dir: &Path, track: &Track) {
    let _ = fs::remove_file(user_file(dir, track));
    let _ = fs::remove_file(cache_file(dir, track));
}

pub fn has_user_version(dir: &Path, track: &Track) -> bool {
    user_file(dir, track).exists()
}

/// Keeps an online answer (or the lack of one) so the next play needs no network.
fn remember(dir: &Path, track: &Track, lyrics: Option<&Lyrics>, origin: Origin) {
    let (synced, plain) = match lyrics {
        Some(l @ Lyrics::Synced(_)) => (Some(l.to_text()), None),
        Some(Lyrics::Plain(text)) => (None, Some(text.clone())),
        None => (None, None),
    };
    // a failed cache write only costs a repeat lookup next time
    let _ = fs::create_dir_all(dir).and_then(|()| {
        fs::write(
            cache_file(dir, track),
            json!({"synced": synced, "plain": plain, "source": origin.key()}).to_string(),
        )
    });
}

/// Which online lyric services may be asked.
#[derive(Clone, Copy, Default)]
pub struct Online {
    /// "Look up lyrics online": nothing is sent anywhere without it.
    pub allowed: bool,
    /// Also ask NetEase when LRCLIB has nothing, and when trying another source.
    pub fallback: bool,
}

/// What each online source said, for the details under a "no lyrics" message.
pub type Notes = Vec<(&'static str, String)>;

/// Where the lyrics on show came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// Typed, imported, or kept from "Try another source" by the user.
    Yours,
    /// A `.lrc` file next to the track.
    Sidecar,
    /// The audio file's own tags.
    Tags,
    Lrclib,
    NetEase,
    /// An online answer remembered before Skratch noted which service gave it.
    Earlier,
}

impl Origin {
    /// For the small line under the lyrics: "from {label}".
    pub fn label(self) -> &'static str {
        match self {
            Self::Yours => "your own lyrics",
            Self::Sidecar => "the .lrc file beside the track",
            Self::Tags => "the file's tags",
            Self::Lrclib => "LRCLIB",
            Self::NetEase => "NetEase",
            Self::Earlier => "an earlier online lookup",
        }
    }

    /// How a remembered online answer names its service.
    fn key(self) -> &'static str {
        match self {
            Self::Lrclib => "lrclib",
            Self::NetEase => "netease",
            _ => "",
        }
    }

    fn from_key(key: Option<&str>) -> Self {
        match key {
            Some("lrclib") => Self::Lrclib,
            Some("netease") => Self::NetEase,
            _ => Self::Earlier,
        }
    }
}

/// The outcome of a lookup: the lyrics, if any, where they came from, and what the sources
/// answered.
pub struct Answer {
    pub lyrics: Option<Lyrics>,
    pub origin: Option<Origin>,
    pub notes: Notes,
}

fn note(source: &'static str, answer: &Result<Option<Lyrics>>, notes: &mut Notes) {
    let said = match answer {
        Ok(Some(Lyrics::Synced(_))) => "found timed lyrics".to_string(),
        Ok(Some(Lyrics::Plain(_))) => "found plain lyrics".to_string(),
        Ok(None) => "nothing matched this track".to_string(),
        Err(error) => format!("could not be reached ({error:#})"),
    };
    notes.push((source, said));
}

fn find(track: &Track, dir: &Path, online: Online) -> Answer {
    let local = |lyrics: Option<Lyrics>, origin| Answer {
        origin: lyrics.as_ref().map(|_| origin),
        lyrics,
        notes: Notes::new(),
    };
    if let Ok(text) = fs::read_to_string(user_file(dir, track)) {
        return local(Lyrics::from_text(&text), Origin::Yours);
    }
    if let Some(lyrics) = fs::read_to_string(track.path.with_extension("lrc"))
        .ok()
        .and_then(|text| Lyrics::from_text(&text))
    {
        return local(Some(lyrics), Origin::Sidecar);
    }
    if let Some(lyrics) = embedded(&track.path) {
        return local(Some(lyrics), Origin::Tags);
    }
    let cache = cache_file(dir, track);
    if let Ok(bytes) = fs::read(&cache)
        && let Ok(saved) = serde_json::from_slice::<Value>(&bytes)
    {
        let text = |key: &str| saved[key].as_str().and_then(Lyrics::from_text);
        if let Some(lyrics) = text("synced").or_else(|| text("plain")) {
            return local(Some(lyrics), Origin::from_key(saved["source"].as_str()));
        }
        let fresh = fs::metadata(&cache)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| SystemTime::now().duration_since(t).ok())
            .is_some_and(|age| age < MISS_TTL);
        if fresh {
            return Answer {
                lyrics: None,
                origin: None,
                notes: vec![(
                    "Earlier lookup",
                    "nothing was found; Skratch asks again after a week".into(),
                )],
            };
        }
    }
    if !online.allowed {
        return local(None, Origin::Earlier);
    }
    let mut notes = Notes::new();
    let primary = lrclib(track);
    note("LRCLIB", &primary, &mut notes);
    // "nothing" is only remembered when every source we asked actually answered
    let mut complete = primary.is_ok();
    let mut lyrics = primary.ok().flatten();
    let mut origin = Origin::Lrclib;
    if lyrics.is_none() && online.fallback {
        let second = netease(track);
        note("NetEase", &second, &mut notes);
        complete &= second.is_ok();
        lyrics = second.ok().flatten();
        origin = Origin::NetEase;
    }
    if lyrics.is_some() || complete {
        remember(dir, track, lyrics.as_ref(), origin);
    }
    Answer {
        origin: lyrics.as_ref().map(|_| origin),
        lyrics,
        notes,
    }
}

/// The first online lyrics that differ from `current`, preferring synced ones, and which
/// service gave them. `Err` only when no source could be reached at all.
fn other_source(track: &Track, current: Option<&Lyrics>) -> Result<Option<(Lyrics, Origin)>> {
    let mut found = Vec::new();
    let mut errors = Vec::new();
    let mut answered = 0;
    for (origin, answer) in [
        (Origin::Lrclib, lrclib(track)),
        (Origin::NetEase, netease(track)),
    ] {
        match answer {
            Ok(lyrics) => {
                answered += 1;
                found.extend(lyrics.filter(|l| current != Some(l)).map(|l| (l, origin)));
            }
            Err(error) => errors.push(error),
        }
    }
    let best = found
        .iter()
        .position(|(l, _)| matches!(l, Lyrics::Synced(_)))
        .unwrap_or(0);
    if !found.is_empty() {
        return Ok(Some(found.swap_remove(best)));
    }
    match errors.into_iter().next() {
        Some(error) if answered == 0 => Err(error),
        _ => Ok(None),
    }
}

pub enum Status {
    Loading,
    Found(Lyrics),
    Missing,
}

/// Looks the playing track's lyrics up in the background and holds the answer.
pub struct Lookup {
    tx: Sender<(PathBuf, bool, Answer)>,
    rx: Receiver<(PathBuf, bool, Answer)>,
    track: Option<PathBuf>,
    /// Whether the answer in `status` already included the online lookup.
    asked_online: bool,
    pub status: Status,
    /// Where the lyrics in `status` came from, once they are found.
    pub origin: Option<Origin>,
    /// What each source said when `status` is `Missing`.
    pub notes: Notes,
    /// The synced line the panel last scrolled to.
    pub scrolled: Option<usize>,
    /// The lyrics being typed or imported, while the editor is open.
    pub editing: Option<String>,
    /// Rhyme marks for each line of the lyrics in `status`; worked out on first display.
    pub marks: Option<Vec<Vec<crate::rhyme::Mark>>>,
    /// A `.lrc`/`.txt` file the user is choosing to import.
    pub picking: Option<Receiver<String>>,
    /// "Try another source" in flight: the track, and whether it found different lyrics.
    other: (Sender<Other>, Receiver<Other>),
    pub other_state: OtherState,
}

type Other = (PathBuf, Result<Option<(Lyrics, Origin)>>);

/// Where "Try another source" stands, for the menu to show.
#[derive(Clone, PartialEq)]
pub enum OtherState {
    Idle,
    Searching,
    /// Nothing different turned up (or no source could be reached); the lyrics were left alone.
    NothingElse(String),
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
            origin: None,
            notes: Notes::new(),
            scrolled: None,
            editing: None,
            marks: None,
            picking: None,
            other: mpsc::channel(),
            other_state: OtherState::Idle,
        }
    }
}

impl Lookup {
    /// Starts a lookup for `track` if it changed (or `online` was just allowed and the first
    /// answer was "nothing"), and collects a finished one into `status`.
    pub fn update(&mut self, ctx: &egui::Context, track: &Track, dir: &Path, online: Online) {
        let changed = self.track.as_deref() != Some(track.path.as_path());
        let allowed_now =
            online.allowed && !self.asked_online && matches!(self.status, Status::Missing);
        if changed || allowed_now {
            self.track = Some(track.path.clone());
            self.asked_online = online.allowed;
            self.other_state = OtherState::Idle;
            self.status = Status::Loading;
            self.origin = None;
            self.notes.clear();
            self.scrolled = None;
            self.editing = None;
            self.marks = None;
            self.spawn(ctx, track.clone(), dir.to_path_buf(), online);
        }
        while let Ok((path, result)) = self.other.1.try_recv() {
            if self.track.as_deref() != Some(path.as_path()) {
                continue;
            }
            self.other_state = match result {
                Ok(Some((lyrics, origin))) => {
                    self.marks = None;
                    self.scrolled = None;
                    self.status = Status::Found(lyrics);
                    self.origin = Some(origin);
                    OtherState::Idle
                }
                Ok(None) => OtherState::NothingElse("No other source had different lyrics.".into()),
                Err(error) => {
                    OtherState::NothingElse(format!("Could not reach a source: {error:#}"))
                }
            };
        }
        while let Ok((path, _, answer)) = self.rx.try_recv() {
            if self.track.as_deref() == Some(path.as_path()) {
                self.marks = None;
                self.status = answer.lyrics.map_or(Status::Missing, Status::Found);
                self.origin = answer.origin;
                self.notes = answer.notes;
            }
        }
    }

    /// Asks every online source for this track and swaps in the first lyrics that differ from
    /// the ones showing (synced ones first). When there are none, nothing changes.
    pub fn try_another(&mut self, ctx: &egui::Context, track: &Track, dir: &Path) {
        if self.other_state == OtherState::Searching {
            return;
        }
        self.other_state = OtherState::Searching;
        let current = match &self.status {
            Status::Found(lyrics) => Some(lyrics.clone()),
            _ => None,
        };
        let (tx, ctx) = (self.other.0.clone(), ctx.clone());
        let (track, dir) = (track.clone(), dir.to_path_buf());
        std::thread::spawn(move || {
            let result = other_source(&track, current.as_ref());
            if let Ok(Some((lyrics, origin))) = &result {
                // the user's own file would hide the new lyrics on the next play
                if user_file(&dir, &track).exists() {
                    let _ = save(&dir, &track, &lyrics.to_text());
                } else {
                    remember(&dir, &track, Some(lyrics), *origin);
                }
            }
            let _ = tx.send((track.path, result));
            ctx.request_repaint();
        });
    }

    /// Looks again, e.g. after a network failure.
    pub fn retry(&mut self) {
        self.track = None;
    }

    /// Asks the online sources again even though an earlier "nothing" was remembered.
    pub fn ask_again(&mut self, dir: &Path, track: &Track) {
        let _ = fs::remove_file(cache_file(dir, track));
        self.retry();
    }

    fn spawn(&self, ctx: &egui::Context, track: Track, dir: PathBuf, online: Online) {
        let (tx, ctx) = (self.tx.clone(), ctx.clone());
        std::thread::spawn(move || {
            let result = find(&track, &dir, online);
            let _ = tx.send((track.path, online.allowed, result));
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
    fn netease_results_must_match_length_and_artist_and_drop_credits() {
        let results = json!({"result": {"songs": [
            {"id": 1, "duration": 400000, "artists": [{"name": "Daft Punk"}]},
            {"id": 2, "duration": 181000, "artists": [{"name": "Someone Else"}]},
            {"id": 3, "duration": 180500, "artists": [{"name": "Pharrell"}, {"name": "daft punk"}]},
        ]}});
        assert_eq!(netease_ids(&results, 180.0, "Daft Punk"), vec![3]);
        assert_eq!(netease_ids(&results, 180.0, ""), vec![2, 3]);
        assert_eq!(
            netease_ids(&json!({"code": 200}), 180.0, "x"),
            Vec::<u64>::new()
        );
        let lrc = "[00:00.000] 作词 : Someone\n[00:01.000] 作曲：Someone\n[00:05.000]Hello there";
        assert_eq!(
            Lyrics::from_text(&without_credits(lrc)),
            Some(Lyrics::Synced(vec![(5.0, "Hello there".into())]))
        );
    }

    #[test]
    fn stamping_a_line_replaces_its_old_stamp_and_moves_on() {
        let (text, next) = stamp_line("one\n[00:01.00]two\nthree", 6, 65.5);
        assert_eq!(text, "one\n[01:05.50]two\nthree");
        assert_eq!(next, 18);
        let (text, next) = stamp_line("last", 2, 3.0);
        assert_eq!(text, "[00:03.00]last\n");
        assert_eq!(next, 15);
    }

    #[test]
    fn user_lyrics_beat_a_sidecar_and_empty_means_none() {
        let dir = tempfile::tempdir().unwrap();
        let song = dir.path().join("song.mp3");
        fs::write(dir.path().join("song.lrc"), "[00:05.00]wrong").unwrap();
        let track = Track {
            path: song,
            ..Track::default()
        };
        let cache = dir.path().join("cache");
        save(&cache, &track, "right words").unwrap();
        assert_eq!(
            find(&track, &cache, Online::default()).lyrics,
            Some(Lyrics::Plain("right words".into()))
        );
        save(&cache, &track, "").unwrap();
        assert_eq!(find(&track, &cache, Online::default()).lyrics, None);
        restore(&cache, &track);
        assert_eq!(
            find(&track, &cache, Online::default()).lyrics,
            Some(Lyrics::Synced(vec![(5.0, "wrong".into())]))
        );
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
            find(&track, &dir.path().join("cache"), Online::default()).lyrics,
            Some(Lyrics::Synced(vec![(5.0, "hello".into())]))
        );
    }

    #[test]
    fn lyrics_say_where_they_came_from() {
        let dir = tempfile::tempdir().unwrap();
        let track = Track {
            path: dir.path().join("song.mp3"),
            title: "Song".into(),
            artist: "Band".into(),
            duration: 100.0,
            ..Track::default()
        };
        let cache = dir.path().join("cache");
        let origin = |track: &Track| find(track, &cache, Online::default()).origin;
        // an online answer is remembered along with the service that gave it
        remember(
            &cache,
            &track,
            Some(&Lyrics::Plain("words".into())),
            Origin::NetEase,
        );
        assert_eq!(origin(&track), Some(Origin::NetEase));
        // one remembered before the service was noted
        fs::write(cache_file(&cache, &track), r#"{"plain":"words"}"#).unwrap();
        assert_eq!(origin(&track), Some(Origin::Earlier));
        // a .lrc file beside the track comes first, then the user's own lyrics
        fs::write(dir.path().join("song.lrc"), "[00:05.00]hello").unwrap();
        assert_eq!(origin(&track), Some(Origin::Sidecar));
        save(&cache, &track, "mine").unwrap();
        assert_eq!(origin(&track), Some(Origin::Yours));
        // no lyrics, no origin
        save(&cache, &track, "").unwrap();
        assert_eq!(origin(&track), None);
        assert_eq!(Origin::NetEase.label(), "NetEase");
    }
}
