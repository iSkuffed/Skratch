//! Fill tags from a filename using a pattern like `{artist} - {title}`.
//!
//! Placeholders: `{artist}`, `{title}`, `{album}`, `{track}`. A featured-artist credit
//! (`ft.`, `feat.`, `featuring`, bare or in brackets) is detected on its own and appended
//! to the title as `(feat. X)`, so patterns never need to mention it.
use crate::model::MetadataEdit;

pub const DEFAULT_PATTERN: &str = "{artist} - {title}";

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Parsed {
    pub artist: Option<String>,
    pub title: Option<String>,
    pub album: Option<String>,
    pub track_number: Option<u32>,
}

impl Parsed {
    /// `base` with every field the pattern found replaced. The filename is never touched.
    pub fn apply(&self, base: &MetadataEdit) -> MetadataEdit {
        let mut edit = base.clone();
        if let Some(artist) = &self.artist {
            edit.artist = artist.clone();
        }
        if let Some(title) = &self.title {
            edit.title = title.clone();
        }
        if let Some(album) = &self.album {
            edit.album = album.clone();
        }
        if let Some(track) = self.track_number {
            edit.track_number = Some(track);
        }
        edit
    }
}

enum Token<'a> {
    Literal(&'a str),
    Field(&'a str),
}

fn tokens(pattern: &str) -> Option<Vec<Token<'_>>> {
    let mut out = Vec::new();
    let mut rest = pattern;
    while !rest.is_empty() {
        match rest.find('{') {
            Some(0) => {
                let end = rest.find('}')?;
                out.push(Token::Field(&rest[1..end]));
                rest = &rest[end + 1..];
            }
            Some(start) => {
                out.push(Token::Literal(&rest[..start]));
                rest = &rest[start..];
            }
            None => {
                out.push(Token::Literal(rest));
                break;
            }
        }
    }
    // two fields in a row have no separator to split on
    let adjacent = out
        .windows(2)
        .any(|w| matches!(w, [Token::Field(_), Token::Field(_)]));
    (!adjacent && out.iter().any(|t| matches!(t, Token::Field(_)))).then_some(out)
}

/// Pulls a featured-artist credit out of `stem`, returning the stem without it.
fn split_feat(stem: &str) -> (String, Option<String>) {
    const MARKERS: [&str; 6] = ["ft.", "feat.", "featuring", "ft ", "feat ", "with "];
    let lower = stem.to_lowercase();
    let mut best: Option<(usize, usize, bool)> = None; // (marker start, text start, bracketed)
    for marker in MARKERS {
        for (at, _) in lower.match_indices(marker) {
            let before = &stem[..at];
            let bracketed = before.ends_with('(') || before.ends_with('[');
            let boundary = bracketed || before.ends_with(' ');
            // "with" is only a credit inside brackets, never in plain titles
            if !boundary || (marker == "with " && !bracketed) {
                continue;
            }
            let start = if bracketed { at - 1 } else { at };
            if best.is_none_or(|(s, _, _)| start < s) {
                best = Some((start, at + marker.len(), bracketed));
            }
        }
    }
    let Some((start, text, bracketed)) = best else {
        return (stem.to_owned(), None);
    };
    let tail = &stem[text..];
    let end = if bracketed {
        tail.find([')', ']'])
    } else {
        tail.find(" - ")
    }
    .unwrap_or(tail.len());
    let feat = tail[..end]
        .trim()
        .trim_matches(['.', ','])
        .trim()
        .to_owned();
    let after = tail[end..].trim_start_matches([')', ']']);
    let cleaned = format!("{}{}", stem[..start].trim_end(), after);
    (
        cleaned.trim().to_owned(),
        (!feat.is_empty()).then_some(feat),
    )
}

/// Matches `stem` (a filename without extension) against `pattern`.
/// `None` when the pattern is invalid, the stem does not fit it, or a field comes out empty.
pub fn parse(pattern: &str, stem: &str) -> Option<Parsed> {
    let tokens = tokens(pattern)?;
    let (stem, feat) = split_feat(stem);
    let mut rest = stem.as_str();
    let mut parsed = Parsed::default();
    let mut iter = tokens.iter().peekable();
    while let Some(token) = iter.next() {
        match token {
            Token::Literal(lit) => rest = rest.strip_prefix(lit)?,
            Token::Field(name) => {
                let value = match iter.peek() {
                    None => std::mem::take(&mut rest),
                    Some(Token::Literal(lit)) => {
                        let last = iter.clone().nth(1).is_none();
                        let at = if last {
                            rest.strip_suffix(lit).map(str::len)?
                        } else {
                            rest.find(lit)?
                        };
                        let value = &rest[..at];
                        rest = &rest[at..];
                        value
                    }
                    Some(Token::Field(_)) => return None,
                }
                .trim();
                if value.is_empty() {
                    return None;
                }
                match *name {
                    "artist" => parsed.artist = Some(value.to_owned()),
                    "title" => parsed.title = Some(value.to_owned()),
                    "album" => parsed.album = Some(value.to_owned()),
                    "track" => parsed.track_number = Some(value.parse().ok()?),
                    _ => return None,
                }
            }
        }
    }
    if !rest.is_empty() {
        return None;
    }
    if let (Some(title), Some(feat)) = (parsed.title.as_mut(), feat) {
        title.push_str(&format!(" (feat. {feat})"));
    }
    Some(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(pattern: &str, stem: &str) -> Option<(String, String)> {
        let parsed = parse(pattern, stem)?;
        Some((parsed.artist?, parsed.title?))
    }
    fn pair(a: &str, t: &str) -> Option<(String, String)> {
        Some((a.into(), t.into()))
    }

    #[test]
    fn artist_dash_title_with_feature() {
        assert_eq!(
            p("{artist} - {title}", "24kGoldn - Mood ft. Iann Dior"),
            pair("24kGoldn", "Mood (feat. Iann Dior)")
        );
    }

    #[test]
    fn title_dash_artist_with_feature_at_the_end() {
        assert_eq!(
            p("{title} - {artist}", "Mood - 24kGoldn feat. Iann Dior"),
            pair("24kGoldn", "Mood (feat. Iann Dior)")
        );
    }

    #[test]
    fn bracketed_feature_and_plain_names_survive() {
        assert_eq!(
            p(
                "{artist} - {title}",
                "Rod Wave - Heart (feat. Lil Baby) [Remix]"
            ),
            pair("Rod Wave", "Heart [Remix] (feat. Lil Baby)")
        );
        assert_eq!(
            p(
                "{artist} - {title}",
                "Bfb Da Packman x Sada Baby - Free Joe Exotic"
            ),
            pair("Bfb Da Packman x Sada Baby", "Free Joe Exotic")
        );
        // "ft" inside a word is not a credit
        assert_eq!(
            p("{artist} - {title}", "Drift - Soft"),
            pair("Drift", "Soft")
        );
    }

    #[test]
    fn track_numbers_and_misses() {
        let parsed = parse("{track}. {artist} - {title}", "07. Polo G - Deep Wounds").unwrap();
        assert_eq!(parsed.track_number, Some(7));
        assert_eq!(parsed.title.as_deref(), Some("Deep Wounds"));
        assert!(parse("{artist} - {title}", "NoSeparatorHere").is_none());
        assert!(parse("{artist}{title}", "A - B").is_none());
        assert!(parse("{artist} - {title}", " - Title").is_none());
    }
}
