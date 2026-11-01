//! Suggested fixes for a song: tags (and a cover) found online that it lacks. Nothing here
//! changes a file; the caller shows the choices and applies the one the user picks.
use crate::{
    model::{MetadataEdit, SUGGEST_APPLIED, SUGGEST_NEVER, SUGGEST_NOTHING, Track},
    online::{self, Candidate, TagMatch},
    tagparse,
};
use anyhow::Result;
use eframe::egui;
use std::time::Duration;

/// How long "nothing was found" is believed before the song is looked up again.
const NOTHING_TTL: i64 = 7 * 24 * 3600;

/// MusicBrainz asks for at most one request a second.
const PAUSE: Duration = Duration::from_millis(1100);

/// How many different answers one song gets (shown as "1/3").
pub const MOST: usize = 3;

/// One field a suggestion would change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub field: &'static str,
    pub from: String,
    pub to: String,
}

/// One possible answer: tags to write, a cover to embed, or both.
pub struct Choice {
    /// The song's tags with the suggestion filled in; `None` when no tag would change.
    pub edit: Option<MetadataEdit>,
    pub changes: Vec<Change>,
    pub cover: Option<Candidate>,
    /// A small picture of `cover`, when it could be fetched.
    pub thumb: Option<egui::ColorImage>,
}

impl Choice {
    /// The tags to write for `track` as it is now: only the fields this choice changes are
    /// taken, so edits made since the lookup are kept.
    pub fn edit_for(&self, track: &Track) -> Option<MetadataEdit> {
        let wanted = self.edit.as_ref()?;
        let mut edit = MetadataEdit::from(track);
        for change in &self.changes {
            match change.field {
                "Title" => edit.title.clone_from(&wanted.title),
                "Artist" => edit.artist.clone_from(&wanted.artist),
                "Album" => edit.album.clone_from(&wanted.album),
                "Year" => edit.year = wanted.year,
                _ => edit.track_number = wanted.track_number,
            }
        }
        Some(edit)
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
        field,
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

fn thumbnail(candidate: &Candidate) -> Option<egui::ColorImage> {
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
            edit: (!changes.is_empty()).then_some(edit),
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
                edit: None,
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
            strong: true,
        };
        let edit = fill(&song(), &found, false);
        assert_eq!(
            (edit.artist.as_str(), edit.title.as_str()),
            ("Band", "Song")
        );
        let fields: Vec<_> = changes(&song(), &edit)
            .into_iter()
            .map(|c| (c.field, c.to))
            .collect();
        assert_eq!(
            fields,
            [
                ("Album", "Studio".into()),
                ("Year", "2019".into()),
                ("Track", "7".into())
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
            source: "iTunes",
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
            strong: true,
        };
        let edit = fill(&song(), &found, false);
        let choice = Choice {
            changes: changes(&song(), &edit),
            edit: Some(edit),
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
}
