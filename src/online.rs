//! Online cover search: iTunes plus MusicBrainz / Cover Art Archive.
//! Only ever called from a button press, one search at a time.
use anyhow::{Context, Result, ensure};
use serde_json::Value;
use std::{sync::OnceLock, time::Duration};

const USER_AGENT: &str = concat!(
    "Skratch/",
    env!("CARGO_PKG_VERSION"),
    " ( https://github.com/iSkuffed )"
);
const MAX_IMAGE: u64 = 8 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// "Artist - Album (2020)"
    pub label: String,
    pub source: &'static str,
    pub thumb: String,
    pub full: String,
}

fn agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::Agent::config_builder()
            .user_agent(USER_AGENT)
            .timeout_global(Some(Duration::from_secs(15)))
            .build()
            .into()
    })
}

fn get_json(url: &str, query: &[(&str, &str)]) -> Result<Value> {
    let mut request = agent().get(url);
    for (key, value) in query {
        request = request.query(key, value);
    }
    let bytes = request.call()?.body_mut().read_to_vec()?;
    serde_json::from_slice(&bytes).context("unreadable response")
}

/// Downloads an image, refusing anything that is not https or is oversized.
pub fn fetch_image(url: &str) -> Result<Vec<u8>> {
    ensure!(url.starts_with("https://"), "refusing non-https image URL");
    Ok(agent()
        .get(url)
        .call()?
        .body_mut()
        .with_config()
        .limit(MAX_IMAGE)
        .read_to_vec()?)
}

/// Searches every source. Fails only when all of them fail.
pub fn search(artist: &str, album: &str) -> Result<Vec<Candidate>> {
    let (artist, album) = (artist.trim(), album.trim());
    ensure!(
        !artist.is_empty() || !album.is_empty(),
        "type an artist or an album first"
    );
    let apple = itunes(artist, album);
    let brainz = musicbrainz(artist, album);
    if let (Err(a), Err(b)) = (&apple, &brainz) {
        anyhow::bail!("iTunes: {a:#}; MusicBrainz: {b:#}");
    }
    let mut found = apple.unwrap_or_default();
    found.extend(brainz.unwrap_or_default());
    Ok(found)
}

fn itunes(artist: &str, album: &str) -> Result<Vec<Candidate>> {
    let term = format!("{artist} {album}");
    let json = get_json(
        "https://itunes.apple.com/search",
        &[
            ("term", term.trim()),
            ("media", "music"),
            ("entity", "song"),
            ("limit", "20"),
        ],
    )?;
    Ok(parse_itunes(&json))
}

fn parse_itunes(json: &Value) -> Vec<Candidate> {
    let mut seen = Vec::new();
    let mut out = Vec::new();
    for item in json["results"].as_array().into_iter().flatten() {
        let (Some(thumb), Some(artist), Some(album)) = (
            item["artworkUrl100"].as_str(),
            item["artistName"].as_str(),
            item["collectionName"].as_str(),
        ) else {
            continue;
        };
        let year = item["releaseDate"]
            .as_str()
            .and_then(|date| date.get(..4))
            .map(|year| format!(" ({year})"))
            .unwrap_or_default();
        let label = format!("{artist} - {album}{year}");
        // songs from one album share its cover, and reissues repeat the label
        if seen.contains(&thumb.to_owned()) || seen.contains(&label) {
            continue;
        }
        seen.push(thumb.to_owned());
        seen.push(label.clone());
        out.push(Candidate {
            label,
            source: "iTunes",
            thumb: thumb.to_owned(),
            // the CDN serves any size by swapping the name in the URL
            full: thumb.replace("100x100", "600x600"),
        });
    }
    out
}

fn musicbrainz(artist: &str, album: &str) -> Result<Vec<Candidate>> {
    let clean = |text: &str| text.replace(['"', '\\'], " ");
    let mut query = Vec::new();
    if !album.is_empty() {
        query.push(format!("releasegroup:\"{}\"", clean(album)));
    }
    if !artist.is_empty() {
        query.push(format!("artist:\"{}\"", clean(artist)));
    }
    let json = get_json(
        "https://musicbrainz.org/ws/2/release-group/",
        &[
            ("query", &query.join(" AND ")),
            ("fmt", "json"),
            ("limit", "8"),
        ],
    )?;
    Ok(parse_musicbrainz(&json))
}

fn parse_musicbrainz(json: &Value) -> Vec<Candidate> {
    json["release-groups"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|group| {
            let id = group["id"].as_str()?;
            let title = group["title"].as_str()?;
            let artist = group["artist-credit"][0]["name"].as_str().unwrap_or("");
            let year = group["first-release-date"]
                .as_str()
                .and_then(|date| date.get(..4))
                .map(|year| format!(" ({year})"))
                .unwrap_or_default();
            let base = format!("https://coverartarchive.org/release-group/{id}");
            Some(Candidate {
                label: format!("{artist} - {title}{year}"),
                source: "MusicBrainz",
                thumb: format!("{base}/front-250"),
                full: format!("{base}/front-500"),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn itunes_results_are_deduplicated_and_upsized() {
        let art = "https://is1.mzstatic.com/a/100x100bb.jpg";
        let json = json!({"results": [
            {"artworkUrl100": art, "artistName": "A", "collectionName": "B", "releaseDate": "2020-05-01T07:00:00Z"},
            {"artworkUrl100": art, "artistName": "A", "collectionName": "B"},
            {"artistName": "no art", "collectionName": "x"},
        ]});
        let found = parse_itunes(&json);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].label, "A - B (2020)");
        assert_eq!(found[0].full, "https://is1.mzstatic.com/a/600x600bb.jpg");
    }

    #[test]
    fn musicbrainz_groups_become_cover_art_archive_urls() {
        let json = json!({"release-groups": [
            {"id": "abc", "title": "Album", "artist-credit": [{"name": "Band"}], "first-release-date": "1999-03-02"},
            {"title": "no id"},
        ]});
        let found = parse_musicbrainz(&json);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].label, "Band - Album (1999)");
        assert_eq!(
            found[0].thumb,
            "https://coverartarchive.org/release-group/abc/front-250"
        );
    }

    #[test]
    fn image_fetch_refuses_plain_http() {
        assert!(fetch_image("http://example.com/a.jpg").is_err());
    }
}
