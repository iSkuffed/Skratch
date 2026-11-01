//! Finds the library's loose ends: songs that appear twice and tags that look unfinished.
//! It only reads; nothing here changes or deletes a file.
use crate::{model::Track, tagparse};
use std::collections::HashMap;

/// Two lengths this close (in seconds) count as the same recording.
const SAME_LENGTH: f64 = 3.0;

/// Things worth fixing in a song's tags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Problem {
    NoArtist,
    NoAlbum,
    NoYear,
    NoTrackNumber,
    /// The title carries filler like "(Official Video)" or "[HD]".
    NoiseInTitle,
    /// The title is the file's name, with its numbering still on it ("01 - Song").
    NumberInTitle,
}

impl Problem {
    pub const ALL: [Self; 6] = [
        Self::NoArtist,
        Self::NoAlbum,
        Self::NoYear,
        Self::NoTrackNumber,
        Self::NoiseInTitle,
        Self::NumberInTitle,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::NoArtist => "no artist",
            Self::NoAlbum => "no album",
            Self::NoYear => "no year",
            Self::NoTrackNumber => "no track number",
            Self::NoiseInTitle => "filler in the title",
            Self::NumberInTitle => "number in the title",
        }
    }
}

/// Only letters and digits, lowercased, so "Don't Stop" and "dont stop" match.
fn plain(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Groups of tracks (as indexes into `tracks`) that are the same song: same title and artist
/// once filler and punctuation are ignored, and nearly the same length. Biggest groups first.
pub fn duplicates(tracks: &[Track]) -> Vec<Vec<usize>> {
    let mut by_name: HashMap<(String, String), Vec<usize>> = HashMap::new();
    for (i, track) in tracks.iter().enumerate() {
        let title = plain(&tagparse::strip_noise(&track.title));
        if title.is_empty() {
            continue;
        }
        by_name
            .entry((plain(&track.artist), title))
            .or_default()
            .push(i);
    }
    let mut groups = Vec::new();
    for mut same_name in by_name.into_values().filter(|group| group.len() > 1) {
        same_name.sort_by(|a, b| tracks[*a].duration.total_cmp(&tracks[*b].duration));
        // lengths all within SAME_LENGTH of the shortest in the run are one recording
        let mut run: Vec<usize> = Vec::new();
        for i in same_name {
            if run
                .first()
                .is_some_and(|first| tracks[i].duration - tracks[*first].duration > SAME_LENGTH)
            {
                groups.push(std::mem::take(&mut run));
            }
            run.push(i);
        }
        groups.push(run);
    }
    groups.retain(|group: &Vec<usize>| group.len() > 1);
    for group in &mut groups {
        group.sort_by_key(|i| (tracks[*i].album.to_lowercase(), tracks[*i].path.clone()));
    }
    groups.sort_by_key(|group| {
        (
            std::cmp::Reverse(group.len()),
            tracks[group[0]].artist.to_lowercase(),
            tracks[group[0]].title.to_lowercase(),
        )
    });
    groups
}

/// What looks unfinished about this track's tags.
pub fn problems(track: &Track) -> Vec<Problem> {
    let blank = |text: &str| {
        let text = text.trim();
        text.is_empty() || text.eq_ignore_ascii_case("unknown") || text.starts_with("Unknown ")
    };
    let mut found = Vec::new();
    if blank(&track.artist) {
        found.push(Problem::NoArtist);
    }
    if blank(&track.album) {
        found.push(Problem::NoAlbum);
    }
    if track.year.is_none() {
        found.push(Problem::NoYear);
    }
    if track.track_number.is_none() {
        found.push(Problem::NoTrackNumber);
    }
    if tagparse::strip_noise(&track.title).trim() != track.title.trim() {
        found.push(Problem::NoiseInTitle);
    }
    let title = track.title.trim_start();
    let digits = title.chars().take_while(char::is_ascii_digit).count();
    if (1..=3).contains(&digits)
        && title[digits..]
            .trim_start()
            .starts_with(['-', '.', ')', '_'])
    {
        found.push(Problem::NumberInTitle);
    }
    found
}

/// Tracks with at least one of `wanted` wrong (all problems when `wanted` is empty), as indexes
/// into `tracks` with what is wrong with each, worst first.
pub fn untidy(tracks: &[Track], wanted: &[Problem]) -> Vec<(usize, Vec<Problem>)> {
    let mut found: Vec<(usize, Vec<Problem>)> = tracks
        .iter()
        .enumerate()
        .filter_map(|(i, track)| {
            let all = problems(track);
            let shown: Vec<Problem> = all
                .into_iter()
                .filter(|p| wanted.is_empty() || wanted.contains(p))
                .collect();
            (!shown.is_empty()).then_some((i, shown))
        })
        .collect();
    found.sort_by_key(|(i, shown)| (std::cmp::Reverse(shown.len()), *i));
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(artist: &str, title: &str, album: &str, duration: f64) -> Track {
        Track {
            artist: artist.into(),
            title: title.into(),
            album: album.into(),
            duration,
            year: Some(2000),
            track_number: Some(1),
            ..Track::default()
        }
    }

    #[test]
    fn the_same_song_is_found_across_punctuation_filler_and_albums() {
        let tracks = vec![
            track("The Band", "Invented Song", "First Album", 200.0),
            track(
                "the band",
                "Invented Song (Official Video)",
                "Best Of",
                201.5,
            ),
            track("The Band", "Invented Song", "Live", 340.0),
            track("The Band", "Another Tune", "First Album", 200.0),
            track("Someone Else", "Invented Song", "Other", 200.0),
        ];
        let groups = duplicates(&tracks);
        assert_eq!(groups, vec![vec![1, 0]], "ordered by album");
    }

    #[test]
    fn a_chain_of_close_lengths_is_not_one_recording() {
        let tracks: Vec<Track> = [200.0, 203.0, 206.0, 209.0]
            .iter()
            .map(|d| track("The Band", "Invented Song", "Album", *d))
            .collect();
        let groups = duplicates(&tracks);
        // 200 and 203 are within the tolerance of 200; 206 and 209 are not
        assert!(groups.iter().all(|g| g.len() < tracks.len()), "{groups:?}");
        assert_eq!(groups.iter().map(Vec::len).max(), Some(2));
    }

    #[test]
    fn unfinished_tags_are_named() {
        let mut bare = track("", "01 - Invented Song [HD]", "", 100.0);
        bare.year = None;
        bare.track_number = None;
        assert_eq!(problems(&bare).len(), 6);
        assert!(problems(&track("The Band", "Invented Song", "Album", 100.0)).is_empty());
        let tracks = vec![track("A", "Fine", "B", 1.0), bare];
        let found = untidy(&tracks, &[Problem::NoYear]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].0, 1);
    }
}
