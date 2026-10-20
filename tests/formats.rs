//! Real container/codec checks, using only temporary generated silence.
use skratch::{
    library::Library,
    player::{Player, PlayerEvent},
};
use std::{
    path::Path,
    process::Command,
    sync::Arc,
    time::{Duration, Instant},
};

fn ffmpeg(args: &[&str], output: &Path) {
    let result = Command::new("ffmpeg")
        .args(["-nostdin", "-hide_banner", "-loglevel", "error", "-y"])
        .args(args)
        .arg(output)
        .output()
        .expect("ffmpeg is required for the generated format check");
    assert!(
        result.status.success(),
        "{}: {}",
        output.display(),
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
fn generated_common_formats_index_and_play_including_mp4_video_audio() {
    if Command::new("ffmpeg").arg("-version").output().is_err()
        || Command::new("mpv").arg("--version").output().is_err()
    {
        eprintln!("SKIPPED generated format check: install ffmpeg and mpv");
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let music = temp.path().join("music");
    std::fs::create_dir(&music).unwrap();
    let cover = music.join("cover.png");
    image::RgbImage::from_pixel(640, 640, image::Rgb([230_u8, 140, 70]))
        .save(&cover)
        .unwrap();
    for (extension, codec) in [
        ("wav", "pcm_s16le"),
        ("flac", "flac"),
        ("mp3", "libmp3lame"),
        ("ogg", "libvorbis"),
        ("opus", "libopus"),
        ("m4a", "aac"),
        ("aac", "aac"),
        ("aiff", "pcm_s16be"),
    ] {
        ffmpeg(
            &[
                "-f",
                "lavfi",
                "-i",
                "anullsrc=r=48000:cl=mono",
                "-t",
                "2",
                "-c:a",
                codec,
                "-metadata",
                "title=Format fixture",
                "-metadata",
                "artist=Skratch QA",
                "-metadata",
                "album=Generated silence",
            ],
            &music.join(format!("fixture.{extension}")),
        );
    }
    ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "color=c=black:s=64x64:r=5",
            "-f",
            "lavfi",
            "-i",
            "anullsrc=r=48000:cl=mono",
            "-t",
            "2",
            "-c:v",
            "mpeg4",
            "-c:a",
            "aac",
            "-metadata",
            "title=MP4 with video",
        ],
        &music.join("fixture.mp4"),
    );
    let embedded = temp.path().join("embedded.png");
    image::RgbImage::from_pixel(512, 512, image::Rgb([20_u8, 210, 70]))
        .save(&embedded)
        .unwrap();
    ffmpeg(
        &[
            "-i",
            music.join("fixture.mp3").to_str().unwrap(),
            "-i",
            embedded.to_str().unwrap(),
            "-map",
            "0:a",
            "-map",
            "1:v",
            "-c:a",
            "copy",
            "-c:v",
            "png",
            "-id3v2_version",
            "3",
            "-disposition:v",
            "attached_pic",
            "-metadata:s:v",
            "comment=Cover (front)",
        ],
        &music.join("embedded.mp3"),
    );
    let mut library = Library::open(&temp.path().join("data")).unwrap();
    library.add_source(&music).unwrap();
    library.scan(false).unwrap();
    let tracks = library.tracks().unwrap();
    assert_eq!(tracks.len(), 10);
    for track in &tracks {
        let artwork = track.artwork.as_ref().expect("folder cover indexed");
        let image = image::open(artwork).unwrap();
        assert!(image.width() <= 256 && image.height() <= 256);
        if track.path.ends_with("embedded.mp3") {
            let pixel = image.to_rgb8().get_pixel(0, 0).0;
            assert!(
                pixel[1] > 180 && pixel[0] < 50,
                "embedded art takes priority over folder art"
            );
        }
    }
    let mut player = Player::start_with_output(Arc::new(|| {}), Some("null")).unwrap();
    for track in &tracks {
        while player.events.try_recv().is_ok() {}
        player.load(&track.path).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let event = player
                .events
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .expect("playback event");
            match event {
                PlayerEvent::Started => break,
                PlayerEvent::Error(error) => {
                    panic!("{} failed playback: {error}", track.path.display())
                }
                _ => {}
            }
        }
        player.set_paused(true).unwrap();
    }
}
