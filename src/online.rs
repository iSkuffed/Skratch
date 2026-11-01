//! Online cover search: iTunes plus MusicBrainz / Cover Art Archive.
//! Only ever called from a button press, one search at a time.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{sync::OnceLock, time::Duration};

const USER_AGENT: &str = concat!(
    "Skratch/",
    env!("CARGO_PKG_VERSION"),
    " ( https://github.com/iSkuffed )"
);
const MAX_IMAGE: u64 = 8 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Candidate {
    /// "Artist - Album (2020)"
    pub label: String,
    pub source: String,
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

pub(crate) fn get_json(url: &str, query: &[(&str, &str)]) -> Result<Value> {
    let mut request = agent().get(url);
    for (key, value) in query {
        request = request.query(key, value);
    }
    let bytes = request.call()?.body_mut().read_to_vec()?;
    serde_json::from_slice(&bytes).context("unreadable response")
}

/// A web page as text, at most 3 MB of it.
pub(crate) fn get_text(url: &str) -> Result<String> {
    let mut response = agent().get(url).call()?;
    Ok(response
        .body_mut()
        .with_config()
        .limit(3 * 1024 * 1024)
        .read_to_string()?)
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
            source: "iTunes".to_owned(),
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
                source: "MusicBrainz".to_owned(),
                thumb: format!("{base}/front-250"),
                full: format!("{base}/front-500"),
            })
        })
        .collect()
}

/// True when the server said it is busy (503 or 429), so the same request may work shortly.
pub fn is_busy(error: &anyhow::Error) -> bool {
    matches!(
        error.downcast_ref::<ureq::Error>(),
        Some(ureq::Error::StatusCode(503 | 429))
    )
}

/// What MusicBrainz knows about one recording.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagMatch {
    pub artist: String,
    pub title: String,
    pub album: String,
    pub year: Option<u32>,
    pub track_number: Option<u32>,
    /// MusicBrainz id of the album (release group) this is from, to look its first year up.
    pub group: Option<String>,
    /// A high search score and a matching length, so safe to apply without a second look.
    pub strong: bool,
}

/// Looks one track up by artist and title. `seconds` (0 when unknown) is checked against the
/// recording's length. `Ok(None)` when MusicBrainz has nothing.
pub fn lookup_tags(artist: &str, title: &str, seconds: f64) -> Result<Option<TagMatch>> {
    Ok(lookup_all(artist, title, seconds, 1, false)?
        .into_iter()
        .next())
}

/// Up to `most` different answers for one track (best first), for when the first is not the
/// album the user has in mind. With `studio_only` only official studio releases are offered
/// (no live albums, compilations, bootlegs or karaoke), which may leave nothing.
pub fn lookup_all(
    artist: &str,
    title: &str,
    seconds: f64,
    most: usize,
    studio_only: bool,
) -> Result<Vec<TagMatch>> {
    let clean = |text: &str| text.replace(['"', '\\'], " ");
    let mut query = vec![format!("recording:\"{}\"", clean(title.trim()))];
    if !artist.trim().is_empty() {
        query.push(format!("artist:\"{}\"", clean(artist.trim())));
    }
    let json = get_json(
        "https://musicbrainz.org/ws/2/recording/",
        &[
            ("query", &query.join(" AND ")),
            ("fmt", "json"),
            ("limit", "25"),
        ],
    )?;
    Ok(parse_recordings(&json, seconds, most, studio_only))
}

/// The answers worth offering, best first: every release of every recording that fits `seconds`
/// ranked together (official studio albums before compilations and live albums, earlier before
/// later), never the same album twice. When no recording fits the length, the first one is
/// offered anyway (it is not `strong`). At most `most` come back.
fn parse_recordings(json: &Value, seconds: f64, most: usize, studio_only: bool) -> Vec<TagMatch> {
    let fits = |recording: &Value| {
        seconds < 1.0
            || recording["length"]
                .as_f64()
                .is_none_or(|ms| (ms / 1000.0 - seconds).abs() <= 5.0)
    };
    let Some(recordings) = json["recordings"].as_array() else {
        return Vec::new();
    };
    let mut ordered: Vec<&Value> = recordings.iter().filter(|r| fits(r)).collect();
    if ordered.is_empty() {
        ordered.extend(recordings.first());
    }
    // (recording, release); a recording on no release is offered with `Value::Null`
    let mut pairs: Vec<(&Value, &Value)> = Vec::new();
    for recording in ordered {
        match recording["releases"].as_array().filter(|r| !r.is_empty()) {
            Some(releases) => pairs.extend(releases.iter().map(|release| (recording, release))),
            None => pairs.push((recording, &Value::Null)),
        }
    }
    if studio_only {
        pairs.retain(|(_, release)| is_studio(release));
    }
    // stable, so among equals the better-scored recording stays first
    pairs.sort_by_key(|(recording, release)| release_rank(recording, release));
    let mut found: Vec<TagMatch> = Vec::new();
    for (recording, release) in pairs {
        let Some(answer) = recording_match(recording, release, fits(recording)) else {
            continue;
        };
        let same = |f: &TagMatch| f.album.eq_ignore_ascii_case(&answer.album);
        if !found.iter().any(same) {
            found.push(answer);
        }
        if found.len() >= most {
            break;
        }
    }
    found
}

/// An official release of a studio album, single or EP.
fn is_studio(release: &Value) -> bool {
    let group = &release["release-group"];
    release["status"].as_str() == Some("Official")
        && matches!(
            group["primary-type"].as_str(),
            Some("Album" | "Single" | "EP")
        )
        && group["secondary-types"]
            .as_array()
            .is_none_or(|types| types.is_empty())
}

/// Smaller sorts first: the plain recording (not "live, 1986-07-27: ..."), an official studio
/// album, then the earliest.
fn release_rank(recording: &Value, release: &Value) -> (bool, bool, bool, String) {
    let group = &release["release-group"];
    let studio = group["primary-type"].as_str() == Some("Album")
        && group["secondary-types"]
            .as_array()
            .is_none_or(|types| types.is_empty());
    (
        recording["disambiguation"]
            .as_str()
            .is_some_and(|text| !text.is_empty()),
        release["status"].as_str() != Some("Official"),
        !studio,
        release["date"].as_str().unwrap_or("9999").to_owned(),
    )
}

/// `release` is `Value::Null` for a recording that is on no release.
fn recording_match(recording: &Value, release: &Value, fits: bool) -> Option<TagMatch> {
    let artist: String = recording["artist-credit"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|credit| {
            format!(
                "{}{}",
                credit["name"].as_str().unwrap_or(""),
                credit["joinphrase"].as_str().unwrap_or("")
            )
        })
        .collect();
    let date = release["date"].as_str();
    Some(TagMatch {
        artist,
        title: recording["title"].as_str()?.to_owned(),
        album: release["title"].as_str().unwrap_or_default().to_owned(),
        year: date.and_then(|d| d.get(..4)).and_then(|y| y.parse().ok()),
        track_number: release["media"][0]["track"][0]["number"]
            .as_str()
            .and_then(|n| n.parse().ok()),
        group: release["release-group"]["id"].as_str().map(str::to_owned),
        strong: recording["score"].as_u64().unwrap_or(0) >= 90 && fits,
    })
}

/// The year an album (MusicBrainz release group) first came out in, whichever release it was
/// found on. A reissue's own date would be wrong for the song.
pub fn first_release_year(group: &str) -> Result<Option<u32>> {
    ensure!(
        !group.is_empty() && group.chars().all(|c| c.is_ascii_hexdigit() || c == '-'),
        "not a MusicBrainz id"
    );
    let json = get_json(
        &format!("https://musicbrainz.org/ws/2/release-group/{group}"),
        &[("fmt", "json")],
    )?;
    Ok(json["first-release-date"]
        .as_str()
        .and_then(|d| d.get(..4))
        .and_then(|y| y.parse().ok()))
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
    fn recording_prefers_matching_length_and_the_studio_album() {
        let json = json!({"recordings": [
            {"title": "Song", "score": 100, "length": 90000, "artist-credit": [{"name": "A"}]},
            {"title": "Song", "score": 95, "length": 200000,
             "artist-credit": [{"name": "A", "joinphrase": " feat. "}, {"name": "B"}],
             "releases": [
                {"title": "Hits", "status": "Official", "date": "2021-01-01",
                 "release-group": {"primary-type": "Album", "secondary-types": ["Compilation"]}},
                {"title": "Studio", "status": "Official", "date": "2019-05-02",
                 "release-group": {"primary-type": "Album"},
                 "media": [{"track": [{"number": "7"}]}]}]}]});
        let found = parse_recordings(&json, 201.0, 1, false).remove(0);
        assert_eq!(found.artist, "A feat. B");
        assert_eq!(found.album, "Studio");
        assert_eq!((found.year, found.track_number), (Some(2019), Some(7)));
        assert!(found.strong);
        // wrong length: still offered, but not strong
        assert!(!parse_recordings(&json, 50.0, 1, false)[0].strong);
    }

    #[test]
    fn every_release_of_the_recording_is_offered_once_best_first() {
        let json = json!({"recordings": [
            {"title": "Song", "score": 100, "length": 200000, "artist-credit": [{"name": "A"}],
             "releases": [
                {"title": "Hits", "status": "Official", "date": "2021-01-01",
                 "release-group": {"primary-type": "Album", "secondary-types": ["Compilation"]}},
                {"title": "Studio", "status": "Official", "date": "2019-05-02",
                 "release-group": {"primary-type": "Album"}},
                {"title": "Studio", "status": "Official", "date": "2019-05-02",
                 "release-group": {"primary-type": "Album"}},
                {"title": "Bootleg", "status": "Bootleg", "date": "2000-01-01"}]}]});
        let albums: Vec<_> = parse_recordings(&json, 200.0, 3, false)
            .into_iter()
            .map(|m| m.album)
            .collect();
        assert_eq!(albums, ["Studio", "Hits", "Bootleg"]);
        let studio: Vec<_> = parse_recordings(&json, 200.0, 3, true)
            .into_iter()
            .map(|m| m.album)
            .collect();
        assert_eq!(studio, ["Studio"]);
        assert!(parse_recordings(&json!({}), 200.0, 3, false).is_empty());
    }

    #[test]
    fn image_fetch_refuses_plain_http() {
        assert!(fetch_image("http://example.com/a.jpg").is_err());
    }
}
