//! Suggested fixes for a song: tags (and a cover) found online that it lacks. Nothing here
//! changes a file; the caller shows the choices and applies the one the user picks.
use crate::{
    model::{MetadataEdit, SUGGEST_APPLIED, SUGGEST_NEVER, SUGGEST_NOTHING, Track},
    online::{self, Candidate, TagMatch},
    tagparse,
};
use anyhow::Result;
use eframe::egui;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

/// How long "nothing was found" is believed before the song is looked up again.
const NOTHING_TTL: i64 = 7 * 24 * 3600;

/// MusicBrainz asks for at most one request a second.
const PAUSE: Duration = Duration::from_millis(1100);

/// How many different answers one song gets (shown as "1/3").
pub const MOST: usize = 3;

/// One field a suggestion would change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Change {
    pub field: String,
    pub from: String,
    pub to: String,
}

/// One possible answer: tags to write, a cover to embed, or both.
#[derive(Clone, Serialize, Deserialize)]
pub struct Choice {
    pub changes: Vec<Change>,
    pub cover: Option<Candidate>,
    /// A small picture of `cover`, when it could be fetched (not kept between runs).
    #[serde(skip)]
    pub thumb: Option<egui::ColorImage>,
}

impl Choice {
    /// The song's tags with every change taken (`forward`) or put back as they were.
    fn edit_with(&self, track: &Track, forward: bool) -> Option<MetadataEdit> {
        if self.changes.is_empty() {
            return None;
        }
        let mut edit = MetadataEdit::from(track);
        for change in &self.changes {
            let value = if forward { &change.to } else { &change.from };
            match change.field.as_str() {
                "Title" => edit.title.clone_from(value),
                "Artist" => edit.artist.clone_from(value),
                "Album" => edit.album.clone_from(value),
                "Year" => edit.year = value.parse().ok(),
                _ => edit.track_number = value.parse().ok(),
            }
        }
        Some(edit)
    }

    /// The tags to write for `track` as it is now: only the fields this choice changes are
    /// taken, so edits made since the lookup are kept.
    pub fn edit_for(&self, track: &Track) -> Option<MetadataEdit> {
        self.edit_with(track, true)
    }

    /// The tags that put the song back as it was before this choice was applied.
    pub fn undo_for(&self, track: &Track) -> Option<MetadataEdit> {
        self.edit_with(track, false)
    }

    /// Whether this still offers something the song lacks (it may have been fixed by hand
    /// since the lookup).
    pub fn useful_for(&self, track: &Track) -> bool {
        let number = |n: Option<u32>| n.map(|n| n.to_string()).unwrap_or_default();
        let differs = |change: &Change| {
            let now = match change.field.as_str() {
                "Title" => track.title.clone(),
                "Artist" => track.artist.clone(),
                "Album" => track.album.clone(),
                "Year" => number(track.year),
                _ => number(track.track_number),
            };
            now != change.to
        };
        self.changes.iter().any(differs) || (self.cover.is_some() && track.artwork.is_none())
    }
}

/// A suggestion the user took, kept so it can be undone.
#[derive(Clone, Serialize, Deserialize)]
pub struct Applied {
    pub id: i64,
    pub label: String,
    pub choice: Choice,
    /// Whether the tags / the cover were written.
    pub tags: bool,
    pub cover: bool,
}

/// What is remembered between runs: answers waiting for the user, and what was applied.
#[derive(Default, Serialize, Deserialize)]
pub struct Store {
    pub waiting: BTreeMap<i64, Vec<Choice>>,
    pub applied: Vec<Applied>,
}

/// How many applied suggestions can still be undone.
const KEEP_APPLIED: usize = 40;

impl Store {
    fn file(dir: &Path) -> PathBuf {
        dir.join("suggestions.json")
    }

    pub fn load(dir: &Path) -> Self {
        fs::read(Self::file(dir))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    pub fn save(&mut self, dir: &Path) {
        let excess = self.applied.len().saturating_sub(KEEP_APPLIED);
        self.applied.drain(..excess);
        let file = Self::file(dir);
        let temporary = file.with_extension("json.tmp");
        if let Ok(bytes) = serde_json::to_vec(self)
            && fs::write(&temporary, bytes).is_ok()
        {
            let _ = fs::rename(&temporary, &file);
        }
    }
}

/// `track` with the match filled in: empty fields only, or everything with `replace`.
pub fn fill(track: &Track, found: &TagMatch, replace: bool) -> MetadataEdit {
    let mut edit = MetadataEdit::from(track);
    let put = |current: &mut String, new: &str| {
        if !new.is_empty() && (replace || current.trim().is_empty()) {
            *current = new.to_owned();
        }
    };
    put(&mut edit.artist, &found.artist);
    put(&mut edit.title, &found.title);
    put(&mut edit.album, &found.album);
    if found.year.is_some() && (replace || edit.year.is_none()) {
        edit.year = found.year;
    }
    if found.track_number.is_some() && (replace || edit.track_number.is_none()) {
        edit.track_number = found.track_number;
    }
    edit
}

/// What `edit` changes in `track`, field by field.
pub fn changes(track: &Track, edit: &MetadataEdit) -> Vec<Change> {
    let number = |n: Option<u32>| n.map(|n| n.to_string()).unwrap_or_default();
    [
        ("Title", &track.title, &edit.title),
        ("Artist", &track.artist, &edit.artist),
        ("Album", &track.album, &edit.album),
        ("Year", &number(track.year), &number(edit.year)),
        (
            "Track",
            &number(track.track_number),
            &number(edit.track_number),
        ),
    ]
    .into_iter()
    .filter(|(_, from, to)| from != to)
    .map(|(field, from, to)| Change {
        field: field.to_owned(),
        from: from.clone(),
        to: to.clone(),
    })
    .collect()
}

/// Whether the song is worth looking up: something is missing and the user has not already
/// answered (or the last look found nothing less than a week ago).
pub fn wanted(track: &Track, now: i64) -> bool {
    if track.title.trim().is_empty() {
        return false;
    }
    match track.suggest {
        SUGGEST_NEVER | SUGGEST_APPLIED => return false,
        SUGGEST_NOTHING if track.suggest_at.is_some_and(|at| now - at < NOTHING_TTL) => {
            return false;
        }
        _ => {}
    }
    track.album.trim().is_empty()
        || track.year.is_none()
        || track.track_number.is_none()
        || track.artwork.is_none()
}

/// Whether a match is by the artist the song already names (a cover version by someone else
/// is another song). Either side may be missing, and "Queen" fits "Queen feat. Someone".
fn same_artist(have: &str, found: &str) -> bool {
    let plain = |name: &str| -> String {
        name.chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect()
    };
    let (have, found) = (plain(have), plain(found));
    have.is_empty() || found.is_empty() || have.contains(&found) || found.contains(&have)
}

/// The first cover found that is for this album (iTunes and MusicBrainz also return other albums
/// by the artist, and a wrong cover is worse than none).
fn cover_for(list: Vec<Candidate>, album: &str) -> Option<Candidate> {
    let plain = |text: &str| -> String {
        text.chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect()
    };
    let album = plain(album);
    list.into_iter()
        .find(|c| !album.is_empty() && plain(&c.label).contains(&album))
}

/// Puts the album's first year on a match (a reissue's own year would be wrong). A track number
/// belongs to one release, so on a reissue it is dropped rather than guessed.
fn with_first_year(found: &mut TagMatch, first: Option<u32>) {
    if let Some(first) = first
        && found.year.is_none_or(|year| first < year)
    {
        found.year = Some(first);
        found.track_number = None;
    }
}

/// Repeats a request the server answered "busy" to, a few times.
fn patient<T>(mut ask: impl FnMut() -> Result<T>) -> Result<T> {
    for _ in 0..2 {
        match ask() {
            Err(e) if online::is_busy(&e) => std::thread::sleep(Duration::from_secs(3)),
            other => return other,
        }
    }
    ask()
}

pub fn thumbnail(candidate: &Candidate) -> Option<egui::ColorImage> {
    let bytes = online::fetch_image(&candidate.thumb).ok()?;
    let image = image::load_from_memory(&bytes)
        .ok()?
        .thumbnail(160, 160)
        .to_rgba8();
    Some(egui::ColorImage::from_rgba_unmultiplied(
        [image.width() as usize, image.height() as usize],
        image.as_raw(),
    ))
}

/// Looks the song up online (blocking, and slow: it keeps to MusicBrainz's rate limit). Only
/// strong matches become choices. `Err` when the services could not be reached.
pub fn find(track: &Track) -> Result<Vec<Choice>> {
    let title = tagparse::strip_noise(&track.title);
    let matches =
        patient(|| online::lookup_all(&track.artist, &title, track.duration, MOST, true))?;
    let want_cover = track.artwork.is_none();
    let mut choices = Vec::new();
    for found in matches
        .into_iter()
        .filter(|m| m.strong && same_artist(&track.artist, &m.artist))
    {
        let mut found = found;
        if let Some(group) = found.group.clone() {
            std::thread::sleep(PAUSE);
            if let Ok(first) = patient(|| online::first_release_year(&group)) {
                with_first_year(&mut found, first);
            }
        }
        let edit = fill(track, &found, false);
        let changes = changes(track, &edit);
        let album = if found.album.is_empty() {
            &track.album
        } else {
            &found.album
        };
        let cover = if want_cover && !album.is_empty() {
            std::thread::sleep(PAUSE);
            let artist = if track.artist.is_empty() {
                &found.artist
            } else {
                &track.artist
            };
            patient(|| online::search(artist, album))
                .ok()
                .and_then(|list| cover_for(list, album))
        } else {
            None
        };
        if changes.is_empty() && cover.is_none() {
            continue;
        }
        let thumb = cover.as_ref().and_then(thumbnail);
        choices.push(Choice {
            changes,
            cover,
            thumb,
        });
    }
    // no tags to add, but a cover is missing: offer one for the album the song already has
    if choices.is_empty() && want_cover && !track.album.is_empty() {
        std::thread::sleep(PAUSE);
        if let Some(cover) = patient(|| online::search(&track.artist, &track.album))
            .ok()
            .and_then(|list| cover_for(list, &track.album))
        {
            let thumb = thumbnail(&cover);
            choices.push(Choice {
                changes: Vec::new(),
                cover: Some(cover),
                thumb,
            });
        }
    }
    Ok(choices)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn song() -> Track {
        Track {
            id: 1,
            title: "Song".into(),
            artist: "Band".into(),
            duration: 200.0,
            ..Track::default()
        }
    }

    #[test]
    fn a_match_fills_only_what_is_empty_and_lists_each_change() {
        let found = TagMatch {
            artist: "BAND".into(),
            title: "Song (Remastered)".into(),
            album: "Studio".into(),
            year: Some(2019),
            track_number: Some(7),
            group: None,
            strong: true,
        };
        let edit = fill(&song(), &found, false);
        assert_eq!(
            (edit.artist.as_str(), edit.title.as_str()),
            ("Band", "Song")
        );
        let fields: Vec<_> = changes(&song(), &edit)
            .into_iter()
            .map(|c| (c.field.as_str().to_owned(), c.to))
            .collect();
        assert_eq!(
            fields,
            [
                ("Album".to_owned(), "Studio".to_owned()),
                ("Year".to_owned(), "2019".to_owned()),
                ("Track".to_owned(), "7".to_owned())
            ]
        );
        // replacing also takes the other fields
        assert_eq!(changes(&song(), &fill(&song(), &found, true)).len(), 5);
    }

    #[test]
    fn a_cover_version_by_someone_else_is_not_the_same_song() {
        assert!(same_artist("Queen", "Queen"));
        assert!(same_artist("queen", "Queen feat. David Bowie"));
        assert!(same_artist("", "Anyone"));
        assert!(!same_artist("Queen", "Whitney Houston"));
    }

    #[test]
    fn only_a_cover_of_the_same_album_is_offered() {
        let candidate = |label: &str| Candidate {
            label: label.into(),
            source: "iTunes".to_owned(),
            thumb: String::new(),
            full: String::new(),
        };
        let list = vec![
            candidate("Queen - Greatest Hits (1981)"),
            candidate("Queen - A Night At The Opera (1975)"),
        ];
        assert_eq!(
            cover_for(list.clone(), "A Night at the Opera")
                .unwrap()
                .label,
            "Queen - A Night At The Opera (1975)"
        );
        assert!(cover_for(list, "Jazz").is_none());
    }

    #[test]
    fn applying_keeps_edits_made_after_the_lookup() {
        let found = TagMatch {
            artist: "Band".into(),
            title: "Song".into(),
            album: "Studio".into(),
            year: Some(2019),
            track_number: None,
            group: None,
            strong: true,
        };
        let edit = fill(&song(), &found, false);
        let choice = Choice {
            changes: changes(&song(), &edit),
            cover: None,
            thumb: None,
        };
        let mut now = song();
        now.genre = "Rock".into();
        now.track_number = Some(4);
        let applied = choice.edit_for(&now).unwrap();
        assert_eq!(
            (applied.album.as_str(), applied.year),
            ("Studio", Some(2019))
        );
        assert_eq!(
            (applied.genre.as_str(), applied.track_number),
            ("Rock", Some(4))
        );
    }

    #[test]
    fn only_unanswered_songs_with_something_missing_are_wanted() {
        let now = 1_000_000_000;
        let mut track = song();
        assert!(wanted(&track, now));
        track.album = "A".into();
        track.year = Some(2000);
        track.track_number = Some(1);
        track.artwork = Some("cover.png".into());
        assert!(!wanted(&track, now), "nothing is missing");
        track.artwork = None;
        assert!(wanted(&track, now));
        track.suggest = SUGGEST_NEVER;
        assert!(!wanted(&track, now));
        track.suggest = SUGGEST_NOTHING;
        track.suggest_at = Some(now - 3600);
        assert!(!wanted(&track, now), "looked an hour ago");
        track.suggest_at = Some(now - NOTHING_TTL - 1);
        assert!(wanted(&track, now), "a week has passed");
        track.title = " ".into();
        assert!(!wanted(&track, now));
    }

    #[test]
    fn a_reissue_gets_the_albums_first_year_and_loses_its_track_number() {
        let mut found = TagMatch {
            artist: "Band".into(),
            title: "Song".into(),
            album: "Studio".into(),
            year: Some(2011),
            track_number: Some(14),
            group: None,
            strong: true,
        };
        with_first_year(&mut found, Some(1975));
        assert_eq!((found.year, found.track_number), (Some(1975), None));
        let mut original = TagMatch {
            year: Some(1975),
            track_number: Some(3),
            ..found.clone()
        };
        with_first_year(&mut original, Some(1975));
        with_first_year(&mut original, None);
        assert_eq!(
            (original.year, original.track_number),
            (Some(1975), Some(3))
        );
    }

    #[test]
    fn a_choice_can_be_undone_and_is_remembered_between_runs() {
        let found = TagMatch {
            artist: "Band".into(),
            title: "Song".into(),
            album: "Studio".into(),
            year: Some(2019),
            track_number: Some(7),
            group: None,
            strong: true,
        };
        let edit = fill(&song(), &found, false);
        let choice = Choice {
            changes: changes(&song(), &edit),
            cover: None,
            thumb: None,
        };
        let mut done = song();
        done.album = "Studio".into();
        done.year = Some(2019);
        done.track_number = Some(7);
        assert!(choice.useful_for(&song()));
        assert!(!choice.useful_for(&done), "already fixed by hand");
        let back = choice.undo_for(&done).unwrap();
        assert_eq!(
            (back.album.as_str(), back.year, back.track_number),
            ("", None, None)
        );

        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::default();
        store.waiting.insert(9, vec![choice.clone()]);
        store.applied.push(Applied {
            id: 9,
            label: "Band - Song".into(),
            choice,
            tags: true,
            cover: false,
        });
        store.save(dir.path());
        let loaded = Store::load(dir.path());
        assert_eq!(loaded.waiting[&9][0].changes.len(), 3);
        assert_eq!(loaded.applied[0].label, "Band - Song");
        assert!(Store::load(&dir.path().join("nothing")).waiting.is_empty());
    }
}
