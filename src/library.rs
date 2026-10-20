use crate::model::{MetadataEdit, Track};
use anyhow::{Context, Result, bail, ensure};
use image::ImageFormat;
use lofty::{
    config::WriteOptions,
    file::{AudioFile, TaggedFileExt},
    picture::{Picture, PictureType},
    tag::{Accessor, ItemKey, Tag},
};
use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};
use std::{
    collections::{BTreeMap, HashSet},
    ffi::{CString, OsStr},
    fs,
    io::{Cursor, Write},
    os::{raw::c_int, unix::ffi::OsStrExt},
    path::{Component, Path, PathBuf},
    time::UNIX_EPOCH,
};
use tempfile::Builder;
use walkdir::WalkDir;

pub struct Library {
    connection: Connection,
    artwork_dir: PathBuf,
}

#[derive(Debug, Default)]
pub struct ScanReport {
    pub indexed: usize,
    pub warnings: Vec<String>,
}

struct ParsedAudio {
    title: String,
    artist: String,
    album: String,
    album_artist: String,
    genre: String,
    year: Option<u32>,
    track_number: Option<u32>,
    disc_number: Option<u32>,
    duration: f64,
    embedded_art: Option<Vec<u8>>,
}

struct CachedFile {
    id: i64,
    size: i64,
    modified_ns: i64,
    artwork: Option<String>,
    artwork_source: Option<String>,
    artwork_size: Option<i64>,
    artwork_modified_ns: Option<i64>,
}

struct Artwork {
    png: Vec<u8>,
    source: String,
    size: i64,
    modified_ns: i64,
}

impl Library {
    pub fn open(data_dir: &Path) -> Result<Self> {
        fs::create_dir_all(data_dir)
            .with_context(|| format!("create data directory {}", path_label(data_dir)))?;
        let data_dir = fs::canonicalize(data_dir)
            .with_context(|| format!("resolve data directory {}", path_label(data_dir)))?;
        ensure!(
            data_dir.to_str().is_some(),
            "data directory path must be valid UTF-8"
        );

        let artwork_dir = data_dir.join("artwork");
        fs::create_dir_all(&artwork_dir).context("create artwork directory")?;
        let connection = Connection::open(data_dir.join("library.sqlite3"))
            .context("open music library database")?;
        connection
            .execute_batch(
                "PRAGMA foreign_keys = ON;
             PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             CREATE TABLE IF NOT EXISTS sources (
                 path TEXT PRIMARY KEY NOT NULL
             );
             CREATE TABLE IF NOT EXISTS tracks (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 path TEXT NOT NULL UNIQUE,
                 size INTEGER NOT NULL,
                 modified_ns INTEGER NOT NULL,
                 title TEXT NOT NULL,
                 artist TEXT NOT NULL,
                 album TEXT NOT NULL,
                 album_artist TEXT NOT NULL,
                 genre TEXT NOT NULL,
                 year INTEGER,
                 track_number INTEGER,
                 disc_number INTEGER,
                 duration REAL NOT NULL,
                 artwork TEXT,
                 artwork_source TEXT,
                 artwork_size INTEGER,
                 artwork_modified_ns INTEGER
             );
             CREATE TABLE IF NOT EXISTS track_sources (
                 track_id INTEGER NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
                 source_path TEXT NOT NULL REFERENCES sources(path) ON DELETE CASCADE,
                 PRIMARY KEY(track_id, source_path)
             );
             CREATE INDEX IF NOT EXISTS track_sources_by_source
                 ON track_sources(source_path);",
            )
            .context("initialize music library database")?;

        Ok(Self {
            connection,
            artwork_dir,
        })
    }

    pub fn sources(&self) -> Result<Vec<PathBuf>> {
        let mut statement = self
            .connection
            .prepare("SELECT path FROM sources ORDER BY path")?;
        let paths = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .map(|row| row.map(PathBuf::from))
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(paths)
    }

    pub fn add_source(&mut self, path: &Path) -> Result<()> {
        let canonical = fs::canonicalize(path)
            .with_context(|| format!("resolve source directory {}", path_label(path)))?;
        ensure!(canonical.is_dir(), "source must be a directory");
        let canonical = path_text(&canonical, "source directory")?;
        self.connection.execute(
            "INSERT OR IGNORE INTO sources(path) VALUES (?1)",
            [canonical],
        )?;
        Ok(())
    }

    pub fn remove_source(&mut self, path: &Path) -> Result<()> {
        let source = source_path_for_removal(path)?;
        let source = path_text(&source, "source directory")?;
        let transaction = self.connection.transaction()?;
        transaction.execute("DELETE FROM sources WHERE path = ?1", [&source])?;
        let artwork = orphan_artwork(&transaction)?;
        transaction.execute(
            "DELETE FROM tracks
             WHERE NOT EXISTS (
                 SELECT 1 FROM track_sources WHERE track_sources.track_id = tracks.id
             )",
            [],
        )?;
        transaction.commit()?;
        for image in artwork {
            let path = PathBuf::from(image);
            if path.starts_with(&self.artwork_dir) {
                let _ = fs::remove_file(path);
            }
        }
        Ok(())
    }

    pub fn tracks(&self) -> Result<Vec<Track>> {
        let mut statement = self.connection.prepare(
            "SELECT id, path, title, artist, album, album_artist, genre, year,
                    track_number, disc_number, duration, artwork
             FROM tracks
             ORDER BY artist COLLATE NOCASE, album COLLATE NOCASE,
                      disc_number, track_number, title COLLATE NOCASE, path",
        )?;
        let tracks = statement
            .query_map([], track_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(tracks)
    }

    pub fn scan(&mut self, force: bool) -> Result<ScanReport> {
        let source_paths = self.sources()?;
        let mut report = ScanReport::default();
        let mut all_found = HashSet::new();
        let mut parse_fallbacks = BTreeMap::<String, usize>::new();
        let transaction = self.connection.transaction()?;

        for source in source_paths {
            let source_text = path_text(&source, "source directory")?;
            match fs::symlink_metadata(&source) {
                Ok(metadata) if metadata.file_type().is_dir() => {}
                Ok(_) => {
                    push_warning(
                        &mut report,
                        format!("source is unavailable or no longer a directory: {source_text}"),
                    );
                    continue;
                }
                Err(error) => {
                    push_warning(
                        &mut report,
                        format!("source is temporarily unavailable: {source_text}: {error}"),
                    );
                    continue;
                }
            }

            let mut complete = true;
            let mut found_here = HashSet::new();
            for entry in WalkDir::new(&source)
                .follow_links(false)
                .into_iter()
                .filter_entry(|entry| {
                    !(entry.file_type().is_dir()
                        && entry
                            .file_name()
                            .to_string_lossy()
                            .starts_with(".skratch-download-"))
                })
            {
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(error) => {
                        complete = false;
                        push_warning(
                            &mut report,
                            format!(
                                "cannot walk source {}: {error}",
                                error
                                    .path()
                                    .map(path_label)
                                    .unwrap_or_else(|| source_text.to_owned())
                            ),
                        );
                        continue;
                    }
                };
                if !entry.file_type().is_file() {
                    continue;
                }

                let file_path = entry.path();
                let file_text = match path_text(file_path, "audio file path") {
                    Ok(path) => path,
                    Err(error) => {
                        complete = false;
                        push_warning(&mut report, format!("{error:#}: {:?}", file_path));
                        continue;
                    }
                };
                if !is_audio_file(file_path) {
                    continue;
                }
                let metadata = match fs::metadata(file_path) {
                    Ok(metadata) if metadata.is_file() => metadata,
                    Ok(_) => continue,
                    Err(error) => {
                        complete = false;
                        push_warning(
                            &mut report,
                            format!("cannot inspect audio file {file_text}: {error}"),
                        );
                        continue;
                    }
                };

                let size = metadata.len().min(i64::MAX as u64) as i64;
                let modified_ns = modified_ns(&metadata);
                all_found.insert(file_text.to_owned());
                found_here.insert(file_text.to_owned());

                let cached = cached_file(&transaction, file_text)?;
                if !force
                    && cached.as_ref().is_some_and(|cached| {
                        cached.size == size
                            && cached.modified_ns == modified_ns
                            && cached_artwork_is_fresh(
                                cached,
                                file_path.parent().unwrap_or(&source),
                            )
                    })
                {
                    let cached = cached.expect("cache presence checked above");
                    add_track_source(&transaction, cached.id, source_text)?;
                    continue;
                }

                let parsed = match parse_audio(file_path) {
                    Ok(parsed) => parsed,
                    Err(_) => {
                        let extension = file_path
                            .extension()
                            .and_then(OsStr::to_str)
                            .unwrap_or("unknown")
                            .to_ascii_lowercase();
                        *parse_fallbacks.entry(extension).or_default() += 1;
                        fallback_metadata(file_path)
                    }
                };
                let title = nonempty(parsed.title, filename_title(file_path));
                transaction.execute(
                    "INSERT INTO tracks (
                         path, size, modified_ns, title, artist, album, album_artist,
                         genre, year, track_number, disc_number, duration, artwork,
                         artwork_source, artwork_size, artwork_modified_ns
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                               NULL, NULL, NULL, NULL)
                     ON CONFLICT(path) DO UPDATE SET
                         size = excluded.size,
                         modified_ns = excluded.modified_ns,
                         title = excluded.title,
                         artist = excluded.artist,
                         album = excluded.album,
                         album_artist = excluded.album_artist,
                         genre = excluded.genre,
                         year = excluded.year,
                         track_number = excluded.track_number,
                         disc_number = excluded.disc_number,
                         duration = excluded.duration",
                    params![
                        file_text,
                        size,
                        modified_ns,
                        title,
                        parsed.artist,
                        parsed.album,
                        parsed.album_artist,
                        parsed.genre,
                        parsed.year.map(i64::from),
                        parsed.track_number.map(i64::from),
                        parsed.disc_number.map(i64::from),
                        parsed.duration,
                    ],
                )?;
                let track_id: i64 = transaction.query_row(
                    "SELECT id FROM tracks WHERE path = ?1",
                    [&file_text],
                    |row| row.get(0),
                )?;
                add_track_source(&transaction, track_id, source_text)?;

                // a cover chosen through online search stays until the file gets its own
                if cached.as_ref().is_some_and(|cached| {
                    cached.artwork_source.as_deref() == Some("online")
                        && cached
                            .artwork
                            .as_ref()
                            .is_some_and(|art| Path::new(art).is_file())
                }) {
                    continue;
                }

                let artwork = match parsed.embedded_art {
                    Some(bytes) => match make_artwork(&bytes, "embedded".to_owned()) {
                        Ok(Some(artwork)) => Some(artwork),
                        Ok(None) => None,
                        Err(error) => {
                            push_warning(
                                &mut report,
                                format!("cannot decode embedded artwork in {file_text}: {error:#}"),
                            );
                            None
                        }
                    },
                    None => None,
                };
                let artwork = match artwork {
                    Some(artwork) => Some(artwork),
                    None => folder_artwork(file_path.parent().unwrap_or(&source), &mut report),
                };
                let artwork_path = match artwork {
                    Some(artwork) => match store_artwork(&self.artwork_dir, track_id, artwork) {
                        Ok((path, source, size, source_modified_ns)) => {
                            transaction.execute(
                                "UPDATE tracks
                                 SET artwork = ?1, artwork_source = ?2,
                                     artwork_size = ?3, artwork_modified_ns = ?4
                                 WHERE id = ?5",
                                params![path, source, size, source_modified_ns, track_id],
                            )?;
                            Some(PathBuf::from(path))
                        }
                        Err(error) => {
                            push_warning(
                                &mut report,
                                format!("cannot store artwork for {file_text}: {error:#}"),
                            );
                            transaction.execute(
                                "UPDATE tracks SET artwork = NULL, artwork_source = NULL,
                                     artwork_size = NULL, artwork_modified_ns = NULL WHERE id = ?1",
                                [track_id],
                            )?;
                            None
                        }
                    },
                    None => {
                        transaction.execute(
                            "UPDATE tracks SET artwork = NULL, artwork_source = NULL,
                                 artwork_size = NULL, artwork_modified_ns = NULL WHERE id = ?1",
                            [track_id],
                        )?;
                        None
                    }
                };
                if artwork_path.is_none() {
                    let old_art = self.artwork_dir.join(format!("{track_id}.png"));
                    let _ = fs::remove_file(old_art);
                }
            }

            if complete {
                prune_source_tracks(&transaction, source_text, &found_here)?;
            } else {
                push_warning(
                    &mut report,
                    format!("kept prior tracks for partially scanned source {source_text}"),
                );
            }
        }

        transaction.commit()?;
        report.indexed = all_found.len();
        if !parse_fallbacks.is_empty() {
            let summary = parse_fallbacks
                .into_iter()
                .map(|(extension, count)| format!(".{extension}: {count}"))
                .collect::<Vec<_>>()
                .join(", ");
            push_warning(
                &mut report,
                format!(
                    "used filename metadata fallback for audio files Lofty could not parse ({summary})"
                ),
            );
        }
        Ok(report)
    }

    pub fn edit(&mut self, id: i64, edit: &MetadataEdit) -> Result<Track> {
        let track = track_by_id(&self.connection, id)?;
        validate_edit_filename(&track.path, &edit.filename)?;
        let source_metadata = fs::symlink_metadata(&track.path)
            .with_context(|| format!("inspect media file {}", path_label(&track.path)))?;
        ensure!(
            source_metadata.file_type().is_file(),
            "media path is not a regular file"
        );

        let parent = track
            .path
            .parent()
            .context("media path has no parent folder")?;
        let destination = parent.join(&edit.filename);
        let renamed = destination != track.path;
        let tags_unchanged = edit.title == track.title
            && edit.artist == track.artist
            && edit.album == track.album
            && edit.album_artist == track.album_artist
            && edit.genre == track.genre
            && edit.year == track.year
            && edit.track_number == track.track_number
            && edit.disc_number == track.disc_number;
        if renamed {
            match fs::symlink_metadata(&destination) {
                Ok(_) => bail!("destination already exists: {}", path_label(&destination)),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error).context("check rename destination"),
            }
            let destination_text = path_text(&destination, "rename destination")?;
            let occupied: bool = self.connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM tracks WHERE path = ?1 AND id != ?2)",
                params![destination_text, id],
                |row| row.get(0),
            )?;
            ensure!(!occupied, "destination is already in the library");
        }

        if tags_unchanged {
            if !renamed {
                return Ok(track);
            }
            let destination_text = path_text(&destination, "rename destination")?;
            let size = source_metadata.len().min(i64::MAX as u64) as i64;
            let modified_ns = modified_ns(&source_metadata);
            let transaction = self.connection.transaction()?;
            transaction.execute(
                "UPDATE tracks SET path = ?1, size = ?2, modified_ns = ?3 WHERE id = ?4",
                params![destination_text, size, modified_ns, id],
            )?;
            rename_noreplace(&track.path, &destination)
                .with_context(|| format!("rename media file to {}", path_label(&destination)))?;
            if let Err(error) = transaction.commit() {
                return match rename_noreplace(&destination, &track.path) {
                    Ok(()) => Err(error).context("database update failed; original file name was restored"),
                    Err(restore_error) => Err(error).context(format!(
                        "database update failed after rename; media remains at {} and could not be restored: {restore_error}",
                        path_label(&destination)
                    )),
                };
            }
            return track_by_id(&self.connection, id);
        }

        let extension = track
            .path
            .extension()
            .and_then(OsStr::to_str)
            .context("media file has no UTF-8 extension")?;
        let suffix = format!(".{extension}");
        let temporary = Builder::new()
            .prefix(".skratch-edit-")
            .suffix(&suffix)
            .tempfile_in(parent)
            .context("create temporary media file")?;
        fs::copy(&track.path, temporary.path()).context("copy media file for safe editing")?;
        fs::set_permissions(temporary.path(), source_metadata.permissions())
            .context("preserve media file permissions")?;

        let mut tagged_file = lofty::read_from_path(temporary.path())
            .context("this audio format cannot be edited by Lofty")?;
        let tag_type = tagged_file.primary_tag_type();
        ensure!(
            tagged_file.supports_tag_type(tag_type),
            "this audio format does not support tag editing"
        );
        if tagged_file.primary_tag().is_none() {
            tagged_file.insert_tag(Tag::new(tag_type));
        }
        let tag = tagged_file
            .primary_tag_mut()
            .context("audio format has no writable primary tag")?;
        set_tag_text(tag, ItemKey::TrackTitle, &edit.title, "title")?;
        set_tag_text(tag, ItemKey::TrackArtist, &edit.artist, "artist")?;
        set_tag_text(tag, ItemKey::AlbumTitle, &edit.album, "album")?;
        set_tag_text(
            tag,
            ItemKey::AlbumArtist,
            &edit.album_artist,
            "album artist",
        )?;
        set_tag_text(tag, ItemKey::Genre, &edit.genre, "genre")?;
        set_tag_year(tag, edit.year)?;
        set_tag_number(tag, ItemKey::TrackNumber, edit.track_number, "track number")?;
        set_tag_number(tag, ItemKey::DiscNumber, edit.disc_number, "disc number")?;
        tagged_file
            .save_to_path(temporary.path(), WriteOptions::default())
            .context("write edited tags to temporary media file")?;
        temporary
            .as_file()
            .sync_all()
            .context("sync edited media file")?;

        let final_path = if renamed { &destination } else { &track.path };
        let final_path_text = path_text(final_path, "media file path")?;
        let final_metadata =
            fs::metadata(temporary.path()).context("inspect edited temporary media file")?;
        let final_size = final_metadata.len().min(i64::MAX as u64) as i64;
        let final_modified_ns = modified_ns(&final_metadata);
        let transaction = self.connection.transaction()?;
        transaction.execute(
            "UPDATE tracks
             SET path = ?1, size = ?2, modified_ns = ?3,
                 title = ?4, artist = ?5, album = ?6, album_artist = ?7,
                 genre = ?8, year = ?9, track_number = ?10, disc_number = ?11
             WHERE id = ?12",
            params![
                final_path_text,
                final_size,
                final_modified_ns,
                edit.title,
                edit.artist,
                edit.album,
                edit.album_artist,
                edit.genre,
                edit.year.map(i64::from),
                edit.track_number.map(i64::from),
                edit.disc_number.map(i64::from),
                id,
            ],
        )?;

        if renamed {
            rename_noreplace(temporary.path(), &destination)
                .with_context(|| format!("rename media file to {}", path_label(&destination)))?;
        } else {
            fs::rename(temporary.path(), &track.path)
                .context("atomically replace media file with edited copy")?;
        }
        temporary.keep().ok();

        if let Err(error) = transaction.commit() {
            if renamed {
                let cleanup = fs::remove_file(&destination);
                if let Err(cleanup_error) = cleanup {
                    return Err(error).context(format!(
                        "database update failed after rename; new media file remains at {} and cleanup also failed: {cleanup_error}",
                        path_label(&destination)
                    ));
                }
                return Err(error).context("database update failed; original media file was kept");
            }
            return Err(error).context("database update failed after media tags were changed");
        }

        if renamed {
            fs::remove_file(&track.path).with_context(|| {
                format!(
                    "database now points to {}, but the old file {} could not be removed",
                    path_label(&destination),
                    path_label(&track.path)
                )
            })?;
        }
        track_by_id(&self.connection, id)
    }

    /// Gives a track a cover. It is always kept in Skratch's own artwork cache, where scans
    /// leave it alone. With `embed` it is also written into the audio file.
    pub fn set_cover(&mut self, id: i64, image: &[u8], embed: bool) -> Result<()> {
        let mut artwork =
            make_artwork(image, "online".to_owned())?.context("this is not a usable image")?;
        if embed {
            let track = track_by_id(&self.connection, id)?;
            embed_cover(&track.path, image)?;
            let metadata = fs::metadata(&track.path).context("inspect edited media file")?;
            // record the new size/mtime so the scan treats this as our own edit
            self.connection.execute(
                "UPDATE tracks SET size = ?1, modified_ns = ?2 WHERE id = ?3",
                params![
                    metadata.len().min(i64::MAX as u64) as i64,
                    modified_ns(&metadata),
                    id
                ],
            )?;
            artwork.source = "embedded".to_owned();
        }
        let (path, source, size, modified) = store_artwork(&self.artwork_dir, id, artwork)?;
        self.connection.execute(
            "UPDATE tracks SET artwork = ?1, artwork_source = ?2,
                 artwork_size = ?3, artwork_modified_ns = ?4 WHERE id = ?5",
            params![path, source, size, modified, id],
        )?;
        Ok(())
    }
}

/// Writes `image` as the front cover of `path` through a temporary copy, so a failed write
/// cannot damage the original.
fn embed_cover(path: &Path, image: &[u8]) -> Result<()> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("inspect media file {}", path_label(path)))?;
    ensure!(
        metadata.file_type().is_file(),
        "media path is not a regular file"
    );
    let parent = path.parent().context("media path has no parent folder")?;
    let extension = path
        .extension()
        .and_then(OsStr::to_str)
        .context("media file has no UTF-8 extension")?;
    let temporary = Builder::new()
        .prefix(".skratch-edit-")
        .suffix(&format!(".{extension}"))
        .tempfile_in(parent)
        .context("create temporary media file")?;
    fs::copy(path, temporary.path()).context("copy media file for safe editing")?;
    fs::set_permissions(temporary.path(), metadata.permissions())
        .context("preserve media file permissions")?;

    let mut tagged_file = lofty::read_from_path(temporary.path())
        .context("this audio format cannot be edited by Lofty")?;
    let tag_type = tagged_file.primary_tag_type();
    ensure!(
        tagged_file.supports_tag_type(tag_type),
        "this audio format does not support tag editing"
    );
    if tagged_file.primary_tag().is_none() {
        tagged_file.insert_tag(Tag::new(tag_type));
    }
    let tag = tagged_file
        .primary_tag_mut()
        .context("audio format has no writable primary tag")?;
    let mut picture = Picture::from_reader(&mut &image[..]).context("unsupported cover image")?;
    picture.set_pic_type(PictureType::CoverFront);
    tag.remove_picture_type(PictureType::CoverFront);
    tag.push_picture(picture);
    tagged_file
        .save_to_path(temporary.path(), WriteOptions::default())
        .context("write cover to temporary media file")?;
    temporary
        .as_file()
        .sync_all()
        .context("sync edited media file")?;
    fs::rename(temporary.path(), path).context("atomically replace media file with edited copy")?;
    temporary.keep().ok();
    Ok(())
}

fn track_from_row(row: &Row<'_>) -> rusqlite::Result<Track> {
    Ok(Track {
        id: row.get(0)?,
        path: PathBuf::from(row.get::<_, String>(1)?),
        title: row.get(2)?,
        artist: row.get(3)?,
        album: row.get(4)?,
        album_artist: row.get(5)?,
        genre: row.get(6)?,
        year: row
            .get::<_, Option<i64>>(7)?
            .and_then(|value| value.try_into().ok()),
        track_number: row
            .get::<_, Option<i64>>(8)?
            .and_then(|value| value.try_into().ok()),
        disc_number: row
            .get::<_, Option<i64>>(9)?
            .and_then(|value| value.try_into().ok()),
        duration: row.get(10)?,
        artwork: row.get::<_, Option<String>>(11)?.map(PathBuf::from),
    })
}

fn track_by_id(connection: &Connection, id: i64) -> Result<Track> {
    connection
        .query_row(
            "SELECT id, path, title, artist, album, album_artist, genre, year,
                    track_number, disc_number, duration, artwork
             FROM tracks WHERE id = ?1",
            [id],
            track_from_row,
        )
        .optional()?
        .with_context(|| format!("track {id} is not in the library"))
}

fn parse_audio(path: &Path) -> Result<ParsedAudio> {
    let tagged_file = lofty::read_from_path(path).context("read audio metadata")?;
    let tag = tagged_file
        .primary_tag()
        .or_else(|| tagged_file.first_tag());
    let embedded_art = tag.and_then(|tag| {
        tag.pictures()
            .iter()
            .find(|picture| picture.pic_type() == PictureType::CoverFront)
            .or_else(|| tag.pictures().first())
            .map(|picture| picture.data().to_vec())
    });
    let string = |value: Option<std::borrow::Cow<'_, str>>| {
        value.map(|value| value.into_owned()).unwrap_or_default()
    };
    Ok(ParsedAudio {
        title: string(tag.and_then(Accessor::title)),
        artist: string(tag.and_then(Accessor::artist)),
        album: string(tag.and_then(Accessor::album)),
        album_artist: tag
            .and_then(|tag| tag.get_string(&ItemKey::AlbumArtist))
            .unwrap_or_default()
            .to_owned(),
        genre: string(tag.and_then(Accessor::genre)),
        year: tag.and_then(Accessor::year),
        track_number: tag.and_then(Accessor::track),
        disc_number: tag.and_then(Accessor::disk),
        duration: tagged_file.properties().duration().as_secs_f64(),
        embedded_art,
    })
}

fn fallback_metadata(path: &Path) -> ParsedAudio {
    ParsedAudio {
        title: filename_title(path),
        artist: String::new(),
        album: String::new(),
        album_artist: String::new(),
        genre: String::new(),
        year: None,
        track_number: None,
        disc_number: None,
        duration: 0.0,
        embedded_art: None,
    }
}

fn filename_title(path: &Path) -> String {
    path.file_stem()
        .and_then(OsStr::to_str)
        .unwrap_or("Unknown title")
        .to_owned()
}

fn nonempty(value: String, fallback: String) -> String {
    if value.trim().is_empty() {
        fallback
    } else {
        value
    }
}

fn cached_file(transaction: &Transaction<'_>, path: &str) -> Result<Option<CachedFile>> {
    Ok(transaction
        .query_row(
            "SELECT id, size, modified_ns, artwork, artwork_source,
                    artwork_size, artwork_modified_ns
             FROM tracks WHERE path = ?1",
            [path],
            |row| {
                Ok(CachedFile {
                    id: row.get(0)?,
                    size: row.get(1)?,
                    modified_ns: row.get(2)?,
                    artwork: row.get(3)?,
                    artwork_source: row.get(4)?,
                    artwork_size: row.get(5)?,
                    artwork_modified_ns: row.get(6)?,
                })
            },
        )
        .optional()?)
}

fn cached_artwork_is_fresh(cached: &CachedFile, directory: &Path) -> bool {
    if cached
        .artwork
        .as_ref()
        .is_some_and(|path| !Path::new(path).is_file())
    {
        return false;
    }
    match cached.artwork_source.as_deref() {
        Some("embedded" | "online") => true,
        Some(path) => fs::metadata(path).is_ok_and(|metadata| {
            Some(metadata.len().min(i64::MAX as u64) as i64) == cached.artwork_size
                && Some(modified_ns(&metadata)) == cached.artwork_modified_ns
        }),
        None => !cover_candidates(directory)
            .iter()
            .any(|path| path.is_file()),
    }
}

fn add_track_source(transaction: &Transaction<'_>, id: i64, source: &str) -> Result<()> {
    transaction.execute(
        "INSERT OR IGNORE INTO track_sources(track_id, source_path) VALUES (?1, ?2)",
        params![id, source],
    )?;
    Ok(())
}

fn prune_source_tracks(
    transaction: &Transaction<'_>,
    source: &str,
    found: &HashSet<String>,
) -> Result<()> {
    let mut statement = transaction.prepare(
        "SELECT tracks.id, tracks.path FROM tracks
         JOIN track_sources ON track_sources.track_id = tracks.id
         WHERE track_sources.source_path = ?1",
    )?;
    let previous = statement
        .query_map([source], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(statement);
    for (id, path) in previous {
        if !found.contains(&path) {
            transaction.execute(
                "DELETE FROM track_sources WHERE track_id = ?1 AND source_path = ?2",
                params![id, source],
            )?;
            transaction.execute(
                "DELETE FROM tracks WHERE id = ?1 AND NOT EXISTS (
                     SELECT 1 FROM track_sources WHERE track_sources.track_id = tracks.id
                 )",
                [id],
            )?;
        }
    }
    Ok(())
}

fn orphan_artwork(transaction: &Transaction<'_>) -> Result<Vec<String>> {
    let mut statement = transaction.prepare(
        "SELECT artwork FROM tracks
         WHERE NOT EXISTS (
             SELECT 1 FROM track_sources WHERE track_sources.track_id = tracks.id
         ) AND artwork IS NOT NULL",
    )?;
    Ok(statement
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

fn folder_artwork(directory: &Path, report: &mut ScanReport) -> Option<Artwork> {
    for path in cover_candidates(directory) {
        let metadata = match fs::metadata(&path) {
            Ok(metadata) if metadata.is_file() => metadata,
            _ => continue,
        };
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) => {
                push_warning(
                    report,
                    format!("cannot read folder artwork {}: {error}", path_label(&path)),
                );
                continue;
            }
        };
        let source = match path_text(&path, "folder artwork path") {
            Ok(path) => path.to_owned(),
            Err(error) => {
                push_warning(report, format!("{error:#}"));
                continue;
            }
        };
        match make_artwork(&bytes, source) {
            Ok(Some(mut artwork)) => {
                artwork.size = metadata.len().min(i64::MAX as u64) as i64;
                artwork.modified_ns = modified_ns(&metadata);
                return Some(artwork);
            }
            Ok(None) => {}
            Err(error) => push_warning(
                report,
                format!(
                    "cannot decode folder artwork {}: {error:#}",
                    path_label(&path)
                ),
            ),
        }
    }
    None
}

fn cover_candidates(directory: &Path) -> [PathBuf; 3] {
    [
        directory.join("cover.jpg"),
        directory.join("folder.jpg"),
        directory.join("cover.png"),
    ]
}

fn make_artwork(bytes: &[u8], source: String) -> Result<Option<Artwork>> {
    const ARTWORK_MAX_ALLOC: u64 = 128 * 1024 * 1024;
    let mut reader = image::ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
    if reader.format().is_none() {
        return Ok(None);
    }
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(16_384);
    limits.max_image_height = Some(16_384);
    limits.max_alloc = Some(ARTWORK_MAX_ALLOC);
    reader.limits(limits);
    let image = reader.decode()?;
    let image = image.thumbnail(256, 256);
    let mut png = Cursor::new(Vec::new());
    image.write_to(&mut png, ImageFormat::Png)?;
    Ok(Some(Artwork {
        png: png.into_inner(),
        source,
        size: bytes.len().min(i64::MAX as usize) as i64,
        modified_ns: 0,
    }))
}

fn store_artwork(
    artwork_dir: &Path,
    track_id: i64,
    artwork: Artwork,
) -> Result<(String, String, i64, i64)> {
    let path = artwork_dir.join(format!("{track_id}.png"));
    let mut temporary = Builder::new()
        .prefix(".skratch-art-")
        .suffix(".png")
        .tempfile_in(artwork_dir)?;
    temporary.write_all(&artwork.png)?;
    temporary.as_file().sync_all()?;
    fs::rename(temporary.path(), &path).context("install artwork thumbnail")?;
    let path = path_text(&path, "artwork thumbnail path")?.to_owned();
    Ok((path, artwork.source, artwork.size, artwork.modified_ns))
}

fn is_audio_file(path: &Path) -> bool {
    path.extension()
        .and_then(OsStr::to_str)
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "mp3"
                    | "flac"
                    | "wav"
                    | "ogg"
                    | "opus"
                    | "m4a"
                    | "mp4"
                    | "aac"
                    | "aiff"
                    | "aif"
                    | "wma"
                    | "ape"
                    | "wv"
            )
        })
}

fn modified_ns(metadata: &fs::Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}

fn path_text<'a>(path: &'a Path, kind: &str) -> Result<&'a str> {
    path.to_str()
        .with_context(|| format!("{kind} must be valid UTF-8: {:?}", path))
}

fn path_label(path: &Path) -> String {
    path.to_str()
        .map(str::to_owned)
        .unwrap_or_else(|| format!("<non-UTF-8 path: {path:?}>"))
}

fn source_path_for_removal(path: &Path) -> Result<PathBuf> {
    if path.exists() {
        return fs::canonicalize(path)
            .with_context(|| format!("resolve source {}", path_label(path)));
    }
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    Ok(normalized)
}

fn validate_edit_filename(current_path: &Path, filename: &str) -> Result<()> {
    ensure!(!filename.is_empty(), "filename cannot be empty");
    ensure!(!filename.contains('\0'), "filename contains a NUL byte");
    let requested = Path::new(filename);
    let mut components = requested.components();
    ensure!(
        matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none(),
        "filename must be one component in the existing folder"
    );
    ensure!(
        requested.file_name() == Some(OsStr::new(filename)),
        "filename must be one component in the existing folder"
    );
    let current_extension = current_path
        .extension()
        .and_then(OsStr::to_str)
        .context("media file has no UTF-8 extension")?;
    let requested_extension = requested
        .extension()
        .and_then(OsStr::to_str)
        .context("filename must keep the existing extension")?;
    ensure!(
        current_extension.eq_ignore_ascii_case(requested_extension),
        "filename extension must remain unchanged"
    );
    Ok(())
}

fn set_tag_text(tag: &mut Tag, key: ItemKey, value: &str, label: &str) -> Result<()> {
    if value.is_empty() {
        tag.remove_key(&key);
    } else {
        ensure!(
            tag.insert_text(key, value.to_owned()),
            "this tag format cannot store {label}"
        );
    }
    Ok(())
}

fn set_tag_number(tag: &mut Tag, key: ItemKey, value: Option<u32>, label: &str) -> Result<()> {
    match value {
        Some(value) => ensure!(
            tag.insert_text(key, value.to_string()),
            "this tag format cannot store {label}"
        ),
        None => tag.remove_key(&key),
    }
    Ok(())
}

fn set_tag_year(tag: &mut Tag, value: Option<u32>) -> Result<()> {
    match value {
        Some(year) => {
            tag.set_year(year);
            ensure!(
                tag.year() == Some(year),
                "this tag format cannot store year"
            );
        }
        None => tag.remove_year(),
    }
    Ok(())
}

fn push_warning(report: &mut ScanReport, warning: String) {
    const MAX_WARNINGS: usize = 64;
    if report.warnings.len() < MAX_WARNINGS - 1 {
        report.warnings.push(warning);
    } else if report.warnings.len() == MAX_WARNINGS - 1 {
        report
            .warnings
            .push("additional scan warnings omitted".to_owned());
    }
}

fn rename_noreplace(source: &Path, destination: &Path) -> std::io::Result<()> {
    const AT_FDCWD: c_int = -100;
    const RENAME_NOREPLACE: u32 = 1;
    unsafe extern "C" {
        fn renameat2(
            olddirfd: c_int,
            oldpath: *const std::ffi::c_char,
            newdirfd: c_int,
            newpath: *const std::ffi::c_char,
            flags: u32,
        ) -> c_int;
    }
    let source = CString::new(source.as_os_str().as_bytes())?;
    let destination = CString::new(destination.as_os_str().as_bytes())?;
    // RENAME_NOREPLACE makes collision detection atomic with the rename.
    let result = unsafe {
        renameat2(
            AT_FDCWD,
            source.as_ptr(),
            AT_FDCWD,
            destination.as_ptr(),
            RENAME_NOREPLACE,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}
