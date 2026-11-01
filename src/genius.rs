//! Plain lyrics from genius.com. Genius has no lyrics in its API, so this asks its public search
//! for the song's page and reads the lyrics out of that page, which its terms of use do not
//! allow for automated use. It is therefore a last resort that is off until the user turns it
//! on, asks once per lookup, and treats any failure as "this source did not answer".
use crate::{online, tagparse};
use anyhow::Result;
use serde_json::Value;

/// The lyrics of `artist` - `title` as plain text, if Genius has a page for that song.
pub fn lyrics(artist: &str, title: &str) -> Result<Option<String>> {
    let title = tagparse::strip_noise(title);
    let found = online::get_json(
        "https://genius.com/api/search/multi",
        &[("q", &format!("{artist} {title}"))],
    )?;
    for url in song_pages(&found, artist, &title).into_iter().take(2) {
        let page = online::get_text(&url)?;
        let text = extract(&page);
        if !text.is_empty() {
            return Ok(Some(text));
        }
    }
    Ok(None)
}

/// Albums Genius lists for `artist` + `album`, as ("Artist - Album", cover url) with the 1000 px
/// cover. Albums by another artist than the one asked about are left out.
pub fn albums(artist: &str, album: &str) -> Result<Vec<(String, String)>> {
    let found = online::get_json(
        "https://genius.com/api/search/multi",
        &[("q", &format!("{artist} {album}"))],
    )?;
    Ok(album_covers(&found, artist))
}

fn album_covers(results: &Value, artist: &str) -> Vec<(String, String)> {
    let want = plain(artist);
    results["response"]["sections"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|s| s["hits"].as_array().into_iter().flatten())
        .filter(|hit| hit["type"] == "album")
        .filter_map(|hit| {
            let album = &hit["result"];
            let (name, by, cover) = (
                album["name"].as_str()?,
                album["artist"]["name"].as_str()?,
                album["cover_art_url"].as_str()?,
            );
            let by_plain = plain(by);
            let fits = want.is_empty() || by_plain.contains(&want) || want.contains(&by_plain);
            (fits && cover.starts_with("https://")).then(|| (format!("{by} - {name}"), cover.to_owned()))
        })
        .collect()
}

/// Only letters and digits, lowercased, so "Don't Stop" and "dont stop" are the same.
fn plain(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Pages of songs that are this one: the same title, by the same artist when the track has one.
/// That leaves out translations (credited to "Genius Translations"), covers and album pages.
fn song_pages(results: &Value, artist: &str, title: &str) -> Vec<String> {
    let (artist, title) = (plain(artist), plain(title));
    let mut pages: Vec<String> = Vec::new();
    let sections = results["response"]["sections"].as_array();
    for hit in sections
        .into_iter()
        .flatten()
        .flat_map(|s| s["hits"].as_array().into_iter().flatten())
    {
        let song = &hit["result"];
        if hit["type"] != "song" || song["lyrics_state"] != "complete" {
            continue;
        }
        let (Some(url), Some(name)) = (song["url"].as_str(), song["title"].as_str()) else {
            continue;
        };
        let by = plain(song["primary_artist"]["name"].as_str().unwrap_or_default());
        let same_artist =
            artist.is_empty() || (!by.is_empty() && (by.contains(&artist) || artist.contains(&by)));
        if plain(name) == title && same_artist && !pages.iter().any(|p| p == url) {
            pages.push(url.to_owned());
        }
    }
    pages
}

/// The lyrics in a song page: the text of its `data-lyrics-container` blocks, line breaks kept,
/// without the page furniture inside them, the `[Verse 1]` headings or the blank-line runs.
fn extract(html: &str) -> String {
    let mut blocks = Vec::new();
    let mut from = 0;
    while let Some(at) = html[from..].find("data-lyrics-container=\"true\"") {
        let start = from + at;
        let Some(open_end) = html[start..].find('>') else {
            break;
        };
        let (text, used) = block(&html[start + open_end + 1..]);
        blocks.push(text);
        from = start + open_end + 1 + used;
    }
    let mut lines: Vec<String> = Vec::new();
    for line in blocks.join("\n").lines() {
        let line = line.trim();
        let heading = line.starts_with('[') && line.ends_with(']');
        let blank = line.is_empty();
        if heading || (blank && lines.last().is_none_or(String::is_empty)) {
            continue;
        }
        lines.push(line.to_owned());
    }
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    lines.join("\n")
}

/// The text inside one container whose opening tag has just been passed, and how much of `html`
/// it took. Parts the page marks `data-exclude-from-selection` (credits, buttons) are left out.
fn block(html: &str) -> (String, usize) {
    let (mut text, mut depth, mut skip_at, mut at) = (String::new(), 1, None, 0);
    while let Some(lt) = html[at..].find('<') {
        let lt = at + lt;
        if skip_at.is_none() {
            text.push_str(&decode(&html[at..lt]));
        }
        let Some(gt) = tag_end(&html[lt..]) else {
            break;
        };
        let tag = &html[lt + 1..lt + gt];
        at = lt + gt + 1;
        let name = tag
            .trim_start_matches('/')
            .split(|c: char| c.is_whitespace() || c == '/')
            .next()
            .unwrap_or("");
        match (name, tag.starts_with('/')) {
            ("br", _) if skip_at.is_none() => text.push('\n'),
            ("div", false) => {
                depth += 1;
                if skip_at.is_none() && tag.contains("data-exclude-from-selection=\"true\"") {
                    skip_at = Some(depth);
                }
            }
            ("div", true) => {
                if skip_at == Some(depth) {
                    skip_at = None;
                }
                depth -= 1;
                if depth == 0 {
                    return (text, at);
                }
            }
            _ => {}
        }
    }
    (text, html.len())
}

/// Where the tag at the start of `html` ends, not counting a `>` inside a quoted value.
fn tag_end(html: &str) -> Option<usize> {
    let mut quote = None;
    for (i, c) in html.char_indices() {
        match (c, quote) {
            ('"' | '\'', None) => quote = Some(c),
            (q, Some(open)) if q == open => quote = None,
            ('>', None) => return Some(i),
            _ => {}
        }
    }
    None
}

/// Plain text for the character references pages use.
fn decode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        rest = &rest[amp..];
        let decoded = rest.find(';').filter(|&semi| semi <= 10).and_then(|semi| {
            let name = &rest[1..semi];
            let ch = match name {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                "nbsp" => Some(' '),
                _ => name
                    .strip_prefix("#x")
                    .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                    .or_else(|| name.strip_prefix('#').and_then(|n| n.parse().ok()))
                    .and_then(char::from_u32),
            };
            ch.map(|c| (c, semi + 1))
        });
        match decoded {
            Some((c, used)) => {
                out.push(c);
                rest = &rest[used..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn album_hits_give_covers_for_the_artist_asked_about() {
        let hit = |name: &str, by: &str, cover: &str| {
            json!({"type": "album", "result": {"name": name, "artist": {"name": by}, "cover_art_url": cover}})
        };
        let results = json!({"response": {"sections": [
            {"type": "song", "hits": [{"type": "song", "result": {"title": "x"}}]},
            {"type": "album", "hits": [
                hit("Blue", "The Band", "https://images.example/a.1000x1000x1.jpg"),
                hit("Blue", "Somebody Else", "https://images.example/b.1000x1000x1.jpg"),
                hit("Plain", "The Band", "http://images.example/c.jpg"),
            ]},
        ]}});
        assert_eq!(
            album_covers(&results, "the band"),
            [("The Band - Blue".to_string(), "https://images.example/a.1000x1000x1.jpg".to_string())]
        );
        assert_eq!(album_covers(&results, "").len(), 2);
    }

    #[test]
    fn lyrics_are_read_out_of_the_page_without_its_furniture() {
        let page = concat!(
            "<html><div data-lyrics-container=\"true\" class=\"Lyrics\">",
            "<div data-exclude-from-selection=\"true\" class=\"Header\"><button>12 Contributors<svg><path d=\"M1\"/></svg></button><div>Song Lyrics</div></div>",
            "[Verse 1]<br/>It&#x27;s a <a href=\"/x\"><span>made-up</span></a> line &amp; more<br/><br/>",
            "[Chorus]<br/><i>Second</i> invented line</div>",
            "<div class=\"Ad\">not lyrics</div>",
            "<div data-lyrics-container=\"true\">Third invented line<br/>Fourth, with a > in it</div></html>",
        );
        assert_eq!(
            extract(page),
            "It's a made-up line & more\n\nSecond invented line\nThird invented line\nFourth, with a > in it"
        );
        assert_eq!(extract("<html>no lyrics here</html>"), "");
    }

    #[test]
    fn only_the_same_song_by_the_same_artist_counts() {
        let hit = |kind: &str, title: &str, artist: &str, url: &str, state: &str| {
            json!({"type": kind, "result": {"title": title, "url": url, "lyrics_state": state,
                "primary_artist": {"name": artist}}})
        };
        let results = json!({"response": {"sections": [
            {"type": "top_hit", "hits": [hit("album", "Invented Song", "The Band", "https://genius.com/albums/a", "complete")]},
            {"type": "song", "hits": [
                hit("song", "Invented Song (English Translation)", "Genius English Translations", "https://genius.com/t", "complete"),
                hit("song", "Invented Song", "Someone Else", "https://genius.com/else", "complete"),
                hit("song", "Invented Song", "The Band", "https://genius.com/unreleased", "unreleased"),
                hit("song", "Invented Song", "The Band", "https://genius.com/band-invented-song-lyrics", "complete"),
                hit("song", "Invented Song", "The Band", "https://genius.com/band-invented-song-lyrics", "complete"),
            ]},
        ]}});
        let pages = song_pages(&results, "The Band", "Invented Song");
        assert_eq!(pages, ["https://genius.com/band-invented-song-lyrics"]);
        // a track with no artist tag is matched on its title alone
        assert_eq!(song_pages(&results, "", "invented song!").len(), 2);
    }

    /// Asks the real genius.com, so it only runs on request: `cargo test -- --ignored genius`.
    #[test]
    #[ignore = "needs the network"]
    fn live_lookup_gives_clean_lyrics() {
        for (artist, title) in [
            ("Radiohead", "Karma Police"),
            ("Daft Punk", "Get Lucky (Official Video)"),
            ("Nobody Real", "Nothing Here Xyzzy"),
        ] {
            let found = lyrics(artist, title).unwrap();
            match found {
                Some(text) => {
                    println!(
                        "{artist} - {title}: {} lines, {} bytes, starts {:?}, ends {:?}",
                        text.lines().count(),
                        text.len(),
                        text.lines()
                            .next()
                            .map(|l| l.chars().take(14).collect::<String>()),
                        text.lines()
                            .last()
                            .map(|l| l.chars().take(14).collect::<String>()),
                    );
                    assert!(text.lines().count() > 10);
                    for bad in [
                        "<",
                        "Contributors",
                        "[Verse",
                        "[Chorus",
                        "&#",
                        "Embed",
                        "You might also like",
                    ] {
                        assert!(!text.contains(bad), "{bad:?} left in the text of {title}");
                    }
                    assert!(text.lines().all(|l| l == l.trim()));
                }
                None => println!("{artist} - {title}: nothing"),
            }
        }
    }
}
