use image::{Rgb, RgbImage};
use lofty::{file::TaggedFileExt, read_from_path, tag::Accessor};
use skratch::{library::Library, model::MetadataEdit};
use std::{fs, path::Path, process::Command};
use tempfile::tempdir;

fn write_wav(path: &Path) {
    let samples = vec![0u8; 16_000];
    let mut wav = Vec::with_capacity(44 + samples.len());
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36u32 + samples.len() as u32).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&8_000u32.to_le_bytes());
    wav.extend_from_slice(&16_000u32.to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&(samples.len() as u32).to_le_bytes());
    wav.extend_from_slice(&samples);
    fs::write(path, wav).expect("write synthetic WAV");
}

fn edit_for(filename: &str) -> MetadataEdit {
    MetadataEdit {
        title: "Edited Title".to_owned(),
        artist: "Example Artist".to_owned(),
        album: "Example Album".to_owned(),
        album_artist: String::new(),
        genre: "Electronic".to_owned(),
        year: Some(2024),
        track_number: Some(3),
        disc_number: None,
        filename: filename.to_owned(),
    }
}

#[test]
fn scan_persists_sources_and_tracks_across_overlapping_roots() {
    let temp = tempdir().unwrap();
    let data = temp.path().join("data");
    let outer = temp.path().join("music");
    let nested = outer.join("nested");
    fs::create_dir_all(&nested).unwrap();
    let media = nested.join("song.wav");
    write_wav(&media);

    let mut library = Library::open(&data).unwrap();
    library.add_source(&outer).unwrap();
    library.add_source(&nested).unwrap();
    let report = library.scan(false).unwrap();
    assert_eq!(report.indexed, 1);
    assert_eq!(library.tracks().unwrap().len(), 1);
    let id = library.tracks().unwrap()[0].id;

    drop(library);
    let mut library = Library::open(&data).unwrap();
    assert_eq!(library.sources().unwrap().len(), 2);
    assert_eq!(library.tracks().unwrap()[0].id, id);

    library.remove_source(&outer).unwrap();
    assert_eq!(library.tracks().unwrap().len(), 1);
    library.remove_source(&nested).unwrap();
    assert!(library.tracks().unwrap().is_empty());
}

#[test]
fn offline_roots_keep_tracks_and_available_roots_prune_deleted_files() {
    let temp = tempdir().unwrap();
    let data = temp.path().join("data");
    let music = temp.path().join("music");
    fs::create_dir_all(&music).unwrap();
    write_wav(&music.join("song.wav"));

    let mut library = Library::open(&data).unwrap();
    library.add_source(&music).unwrap();
    library.scan(false).unwrap();
    fs::rename(&music, temp.path().join("offline")).unwrap();
    let report = library.scan(false).unwrap();
    assert_eq!(library.tracks().unwrap().len(), 1);
    assert!(!report.warnings.is_empty());

    fs::rename(temp.path().join("offline"), &music).unwrap();
    fs::remove_file(music.join("song.wav")).unwrap();
    library.scan(false).unwrap();
    assert!(library.tracks().unwrap().is_empty());
}

#[test]
fn edits_tags_and_renames_without_changing_the_track_id() {
    let temp = tempdir().unwrap();
    let data = temp.path().join("data");
    let music = temp.path().join("music");
    fs::create_dir_all(&music).unwrap();
    write_wav(&music.join("song.wav"));

    let mut library = Library::open(&data).unwrap();
    library.add_source(&music).unwrap();
    library.scan(false).unwrap();
    let before = library.tracks().unwrap().remove(0);
    let edited = library.edit(before.id, &edit_for("renamed.WAV")).unwrap();

    assert_eq!(edited.id, before.id);
    assert_eq!(edited.path, music.join("renamed.WAV"));
    assert_eq!(edited.title, "Edited Title");
    assert_eq!(edited.artist, "Example Artist");
    let tagged = read_from_path(&edited.path).unwrap();
    let tag = tagged.primary_tag().unwrap();
    assert_eq!(tag.title().as_deref(), Some("Edited Title"));
    assert_eq!(tag.artist().as_deref(), Some("Example Artist"));

    drop(library);
    let mut library = Library::open(&data).unwrap();
    assert_eq!(library.tracks().unwrap()[0].id, before.id);
    assert_eq!(library.tracks().unwrap()[0].path, edited.path);
    library.scan(false).unwrap();
    assert_eq!(library.tracks().unwrap()[0].id, before.id);
}

#[test]
fn collision_and_invalid_filename_leave_media_bytes_unchanged() {
    let temp = tempdir().unwrap();
    let data = temp.path().join("data");
    let music = temp.path().join("music");
    fs::create_dir_all(&music).unwrap();
    let source = music.join("song.wav");
    let collision = music.join("taken.wav");
    write_wav(&source);
    write_wav(&collision);
    let original_bytes = fs::read(&source).unwrap();
    let collision_bytes = fs::read(&collision).unwrap();

    let mut library = Library::open(&data).unwrap();
    library.add_source(&music).unwrap();
    library.scan(false).unwrap();
    let track = library
        .tracks()
        .unwrap()
        .into_iter()
        .find(|track| track.path == source)
        .unwrap();

    assert!(library.edit(track.id, &edit_for("taken.wav")).is_err());
    assert!(library.edit(track.id, &edit_for("../outside.wav")).is_err());
    assert_eq!(fs::read(&source).unwrap(), original_bytes);
    assert_eq!(fs::read(&collision).unwrap(), collision_bytes);
    assert!(!temp.path().join("outside.wav").exists());
}

#[test]
fn fallback_audio_files_are_indexed_with_filename_titles() {
    let temp = tempdir().unwrap();
    let data = temp.path().join("data");
    let music = temp.path().join("music");
    fs::create_dir_all(&music).unwrap();
    fs::write(music.join("Unparsed MP4.mp4"), b"not a parseable MP4").unwrap();

    let mut library = Library::open(&data).unwrap();
    library.add_source(&music).unwrap();
    let report = library.scan(false).unwrap();
    let tracks = library.tracks().unwrap();
    assert_eq!(tracks.len(), 1);
    assert_eq!(tracks[0].title, "Unparsed MP4");
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("filename metadata fallback"))
    );
}

#[test]
fn folder_cover_is_thumbnailed_and_refreshed_when_changed() {
    let temp = tempdir().unwrap();
    let data = temp.path().join("data");
    let music = temp.path().join("music");
    fs::create_dir_all(&music).unwrap();
    write_wav(&music.join("song.wav"));
    RgbImage::from_pixel(512, 128, Rgb([220, 20, 30]))
        .save(music.join("cover.png"))
        .unwrap();

    let mut library = Library::open(&data).unwrap();
    library.add_source(&music).unwrap();
    library.scan(false).unwrap();
    let track = library.tracks().unwrap().remove(0);
    let artwork_path = track.artwork.unwrap();
    let first_thumbnail = fs::read(&artwork_path).unwrap();
    let dimensions = image::image_dimensions(&artwork_path).unwrap();
    assert!(dimensions.0 <= 256 && dimensions.1 <= 256);

    RgbImage::from_pixel(512, 256, Rgb([10, 50, 220]))
        .save(music.join("cover.png"))
        .unwrap();
    library.scan(false).unwrap();
    assert_ne!(fs::read(&artwork_path).unwrap(), first_thumbnail);
}

#[test]
fn filename_only_rename_works_for_fallback_formats_without_changing_bytes() {
    let temp = tempdir().unwrap();
    let data = temp.path().join("data");
    let music = temp.path().join("music");
    fs::create_dir_all(&music).unwrap();
    let source = music.join("Unparsed MP4.mp4");
    fs::write(&source, b"not a parseable MP4").unwrap();
    let original_bytes = fs::read(&source).unwrap();

    let mut library = Library::open(&data).unwrap();
    library.add_source(&music).unwrap();
    library.scan(false).unwrap();
    let track = library.tracks().unwrap().remove(0);
    let mut edit = MetadataEdit::from(&track);
    edit.filename = "Renamed.mp4".to_owned();

    let renamed = library.edit(track.id, &edit).unwrap();
    assert_eq!(renamed.id, track.id);
    assert_eq!(renamed.path, music.join("Renamed.mp4"));
    assert_eq!(fs::read(&renamed.path).unwrap(), original_bytes);
    assert!(!source.exists());
}

#[test]
fn clearing_year_from_mp3_and_m4a_persists_after_rescan() {
    if Command::new("ffmpeg").arg("-version").output().is_err() {
        eprintln!("skipping MP3/M4A year round-trip: ffmpeg is unavailable");
        return;
    }

    let temp = tempdir().unwrap();
    let data = temp.path().join("data");
    let music = temp.path().join("music");
    fs::create_dir_all(&music).unwrap();
    for (extension, codec) in [("mp3", "libmp3lame"), ("m4a", "aac")] {
        let path = music.join(format!("year.{extension}"));
        let status = Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "anullsrc=r=8000:cl=mono",
                "-t",
                "0.2",
                "-metadata",
                "date=1999",
                "-codec:a",
                codec,
                "-y",
            ])
            .arg(&path)
            .status()
            .unwrap();
        assert!(
            status.success(),
            "ffmpeg could not create {extension} fixture"
        );
    }

    let mut library = Library::open(&data).unwrap();
    library.add_source(&music).unwrap();
    library.scan(false).unwrap();
    for mut track in library.tracks().unwrap() {
        assert_eq!(
            track.year,
            Some(1999),
            "year missing for {}",
            track.path.display()
        );
        let mut edit = MetadataEdit::from(&track);
        edit.year = None;
        track = library.edit(track.id, &edit).unwrap();
        assert_eq!(track.year, None);
        let tagged = read_from_path(&track.path).unwrap();
        assert_eq!(
            tagged.primary_tag().and_then(Accessor::year),
            None,
            "year remained in {}",
            track.path.display()
        );
    }
    library.scan(true).unwrap();
    assert!(
        library
            .tracks()
            .unwrap()
            .iter()
            .all(|track| track.year.is_none())
    );
}

#[test]
fn unfinished_downloads_are_hidden_until_published() {
    let temp = tempdir().unwrap();
    let music = temp.path().join("music");
    let staging = music.join(".skratch-download-fixture");
    fs::create_dir_all(&staging).unwrap();
    write_wav(&staging.join("unfinished.wav"));
    let mut library = Library::open(&temp.path().join("data")).unwrap();
    library.add_source(&music).unwrap();
    library.scan(false).unwrap();
    assert!(library.tracks().unwrap().is_empty());
    fs::rename(staging.join("unfinished.wav"), music.join("finished.wav")).unwrap();
    library.scan(false).unwrap();
    assert_eq!(library.tracks().unwrap().len(), 1);
}

fn cover_png(color: [u8; 3]) -> Vec<u8> {
    let mut png = std::io::Cursor::new(Vec::new());
    RgbImage::from_pixel(600, 600, Rgb(color))
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
    png.into_inner()
}

#[test]
fn online_cover_stays_in_the_cache_and_survives_rescans() {
    let temp = tempdir().unwrap();
    let music = temp.path().join("music");
    fs::create_dir_all(&music).unwrap();
    write_wav(&music.join("song.wav"));
    // a folder cover would normally win; the chosen cover must beat it
    RgbImage::from_pixel(64, 64, Rgb([220, 20, 30]))
        .save(music.join("cover.png"))
        .unwrap();
    let before = fs::read(music.join("song.wav")).unwrap();

    let mut library = Library::open(&temp.path().join("data")).unwrap();
    library.add_source(&music).unwrap();
    library.scan(false).unwrap();
    let id = library.tracks().unwrap()[0].id;

    library
        .set_cover(id, &cover_png([10, 50, 220]), false)
        .unwrap();
    for force in [false, true] {
        library.scan(force).unwrap();
        let art = library.tracks().unwrap()[0].artwork.clone().unwrap();
        let pixel = image::open(&art).unwrap().to_rgb8().get_pixel(5, 5).0;
        assert!(
            pixel[2] > 150 && pixel[0] < 60,
            "online cover was replaced: {pixel:?}"
        );
    }
    assert_eq!(
        fs::read(music.join("song.wav")).unwrap(),
        before,
        "file must be untouched"
    );
    assert!(library.set_cover(id, b"not an image", false).is_err());
}

#[test]
fn embedding_a_cover_writes_it_into_the_file_and_keeps_the_tags() {
    if Command::new("ffmpeg").arg("-version").output().is_err() {
        eprintln!("skipping cover embed: ffmpeg is unavailable");
        return;
    }
    let temp = tempdir().unwrap();
    let music = temp.path().join("music");
    fs::create_dir_all(&music).unwrap();
    for (name, codec) in [
        ("a.mp3", "libmp3lame"),
        ("b.flac", "flac"),
        ("c.m4a", "aac"),
    ] {
        let status = Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "anullsrc=r=8000:cl=mono",
                "-t",
                "0.2",
                "-metadata",
                "title=Keep Me",
                "-codec:a",
                codec,
                "-y",
            ])
            .arg(music.join(name))
            .status()
            .unwrap();
        assert!(status.success());
    }
    let mut library = Library::open(&temp.path().join("data")).unwrap();
    library.add_source(&music).unwrap();
    library.scan(false).unwrap();
    for track in library.tracks().unwrap() {
        library
            .set_cover(track.id, &cover_png([200, 40, 40]), true)
            .unwrap();
        let tagged = read_from_path(&track.path).unwrap();
        let tag = tagged.primary_tag().unwrap();
        assert_eq!(
            tag.pictures().len(),
            1,
            "no cover in {}",
            track.path.display()
        );
        assert_eq!(tag.title().as_deref(), Some("Keep Me"));
    }
    library.scan(false).unwrap();
    let tracks = library.tracks().unwrap();
    assert!(
        tracks
            .iter()
            .all(|t| t.artwork.is_some() && t.title == "Keep Me")
    );
    // no leftover temp copies next to the music
    assert!(fs::read_dir(&music).unwrap().all(|e| {
        !e.unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".skratch-edit-")
    }));
}
