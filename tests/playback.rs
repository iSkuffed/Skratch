use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use skratch::media::{DesktopControls, MediaCommand};
use skratch::model::Track;
use skratch::player::{Player, PlayerEvent};
use tempfile::tempdir;

fn mpv_available() -> bool {
    Command::new("mpv").arg("--version").output().is_ok()
}

fn write_silence_wav(path: &Path, seconds: u32) {
    let sample_rate = 22_050u32;
    let channels = 1u16;
    let bits_per_sample = 16u16;
    let data_size = sample_rate * seconds * u32::from(channels) * u32::from(bits_per_sample) / 8;
    let mut file = File::create(path).unwrap();
    file.write_all(b"RIFF").unwrap();
    file.write_all(&(36 + data_size).to_le_bytes()).unwrap();
    file.write_all(b"WAVEfmt ").unwrap();
    file.write_all(&16u32.to_le_bytes()).unwrap();
    file.write_all(&1u16.to_le_bytes()).unwrap();
    file.write_all(&channels.to_le_bytes()).unwrap();
    file.write_all(&sample_rate.to_le_bytes()).unwrap();
    file.write_all(
        &(sample_rate * u32::from(channels) * u32::from(bits_per_sample) / 8).to_le_bytes(),
    )
    .unwrap();
    file.write_all(&(channels * bits_per_sample / 8).to_le_bytes())
        .unwrap();
    file.write_all(&bits_per_sample.to_le_bytes()).unwrap();
    file.write_all(b"data").unwrap();
    file.write_all(&data_size.to_le_bytes()).unwrap();
    let silence = vec![0u8; data_size as usize];
    file.write_all(&silence).unwrap();
}

fn wait_for_event(
    player: &Player,
    timeout: Duration,
    matches: impl Fn(&PlayerEvent) -> bool,
) -> PlayerEvent {
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(!remaining.is_zero(), "timed out waiting for player event");
        let event = player.events.recv_timeout(remaining).unwrap();
        if matches(&event) {
            return event;
        }
        if let PlayerEvent::Error(error) = event {
            panic!("unexpected playback error: {error}");
        }
    }
}

#[test]
fn mpv_ipc_controls_eof_and_cleanup() {
    if !mpv_available() {
        eprintln!("skipping mpv integration test because mpv is not installed");
        return;
    }

    let dir = tempdir().unwrap();
    let short_track = dir.path().join("short.wav");
    let long_track = dir.path().join("long.wav");
    let invalid_track = dir.path().join("invalid.audio");
    write_silence_wav(&short_track, 2);
    write_silence_wav(&long_track, 10);
    std::fs::write(&invalid_track, b"not a media file").unwrap();

    let wakes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let wake_count = wakes.clone();
    let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
        wake_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    });
    let mut player = Player::start_with_output(wake, Some("null")).unwrap();

    assert!(player.load(&dir.path().join("missing.wav")).is_err());
    player.load(&short_track).unwrap();
    wait_for_event(&player, Duration::from_secs(3), |event| {
        matches!(event, PlayerEvent::Started)
    });
    wait_for_event(
        &player,
        Duration::from_secs(2),
        |event| matches!(event, PlayerEvent::Duration(duration) if *duration >= 1.9),
    );
    player.set_paused(true).unwrap();
    wait_for_event(&player, Duration::from_secs(2), |event| {
        matches!(event, PlayerEvent::Paused(true))
    });
    player.seek(0.5).unwrap();
    wait_for_event(
        &player,
        Duration::from_secs(2),
        |event| matches!(event, PlayerEvent::Position(position) if *position >= 0.45),
    );
    player.set_volume(37.0).unwrap();
    wait_for_event(
        &player,
        Duration::from_secs(2),
        |event| matches!(event, PlayerEvent::Volume(volume) if (*volume - 37.0).abs() < 0.1),
    );
    player.set_paused(false).unwrap();
    wait_for_event(&player, Duration::from_secs(2), |event| {
        matches!(event, PlayerEvent::Paused(false))
    });
    wait_for_event(&player, Duration::from_secs(5), |event| {
        matches!(event, PlayerEvent::Ended)
    });

    player.load(&long_track).unwrap();
    wait_for_event(&player, Duration::from_secs(3), |event| {
        matches!(event, PlayerEvent::Started)
    });
    player.stop().unwrap();
    let stop_deadline = Instant::now() + Duration::from_millis(250);
    loop {
        match player
            .events
            .recv_timeout(stop_deadline.saturating_duration_since(Instant::now()))
        {
            Ok(PlayerEvent::Ended) => panic!("manual stop was reported as EOF"),
            Ok(_) => continue,
            Err(_) => break,
        }
    }

    player.load(&invalid_track).unwrap();
    wait_for_event(&player, Duration::from_secs(3), |event| {
        matches!(event, PlayerEvent::Error(_))
    });
    assert!(wakes.load(std::sync::atomic::Ordering::Relaxed) > 0);

    let started = Instant::now();
    drop(player);
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "player drop exceeded its bound"
    );
}

fn gdbus_call(arguments: &[&str]) -> std::process::Output {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let output = Command::new("gdbus").args(arguments).output().unwrap();
        if output.status.success() {
            return output;
        }
        assert!(
            Instant::now() < deadline,
            "gdbus call failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn wait_for_mpris_value(arguments: &[&str], expected: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let output = gdbus_call(arguments);
        let value = String::from_utf8_lossy(&output.stdout).into_owned();
        if value.contains(expected) {
            return value;
        }
        assert!(
            Instant::now() < deadline,
            "MPRIS value did not contain {expected:?}: {value}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn receive_media_command(controls: &DesktopControls) -> MediaCommand {
    controls
        .events
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
}

#[test]
fn mpris_events_on_isolated_dbus() {
    const CHILD_ENV: &str = "SKRATCH_MPRIS_TEST_CHILD";
    if std::env::var_os(CHILD_ENV).is_none() {
        if Command::new("dbus-run-session")
            .arg("--help")
            .output()
            .is_err()
            || Command::new("gdbus").arg("help").output().is_err()
        {
            eprintln!("skipping MPRIS integration test without dbus-run-session and gdbus");
            return;
        }
        let output = Command::new("dbus-run-session")
            .arg("--")
            .arg(std::env::current_exe().unwrap())
            .args(["--exact", "mpris_events_on_isolated_dbus", "--nocapture"])
            .env(CHILD_ENV, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "isolated MPRIS test failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }

    let artwork_dir = tempdir().unwrap();
    let artwork_path = artwork_dir.path().join("cover # ü.png");
    std::fs::write(&artwork_path, b"test artwork").unwrap();
    let track = Track {
        path: artwork_dir.path().join("track # ü.mp3"),
        title: "Test Track".into(),
        artist: "Test Artist".into(),
        album: "Test Album".into(),
        duration: 90.0,
        artwork: Some(artwork_path),
        ..Track::default()
    };
    let mut controls = DesktopControls::new(Arc::new(|| {})).unwrap();
    controls.update(Some(&track), true, 12.5, 42.0).unwrap();

    let metadata = wait_for_mpris_value(
        &[
            "call",
            "--session",
            "--dest",
            "org.mpris.MediaPlayer2.skratch",
            "--object-path",
            "/org/mpris/MediaPlayer2",
            "--method",
            "org.freedesktop.DBus.Properties.Get",
            "org.mpris.MediaPlayer2.Player",
            "Metadata",
        ],
        "Test Track",
    );
    assert!(metadata.contains("cover%20%23%20%C3%BC.png"), "{metadata}");
    let position = wait_for_mpris_value(
        &[
            "call",
            "--session",
            "--dest",
            "org.mpris.MediaPlayer2.skratch",
            "--object-path",
            "/org/mpris/MediaPlayer2",
            "--method",
            "org.freedesktop.DBus.Properties.Get",
            "org.mpris.MediaPlayer2.Player",
            "Position",
        ],
        "12500000",
    );
    assert!(position.contains("12500000"), "{position}");

    gdbus_call(&[
        "call",
        "--session",
        "--dest",
        "org.mpris.MediaPlayer2.skratch",
        "--object-path",
        "/org/mpris/MediaPlayer2",
        "--method",
        "org.mpris.MediaPlayer2.Player.PlayPause",
    ]);
    assert_eq!(receive_media_command(&controls), MediaCommand::Toggle);

    gdbus_call(&[
        "call",
        "--session",
        "--dest",
        "org.mpris.MediaPlayer2.skratch",
        "--object-path",
        "/org/mpris/MediaPlayer2",
        "--method",
        "org.mpris.MediaPlayer2.Player.Next",
    ]);
    assert_eq!(receive_media_command(&controls), MediaCommand::Next);

    gdbus_call(&[
        "call",
        "--session",
        "--dest",
        "org.mpris.MediaPlayer2.skratch",
        "--object-path",
        "/org/mpris/MediaPlayer2",
        "--method",
        "org.mpris.MediaPlayer2.Player.Seek",
        "5000000",
    ]);
    assert_eq!(receive_media_command(&controls), MediaCommand::SeekBy(5.0));

    gdbus_call(&[
        "call",
        "--session",
        "--dest",
        "org.mpris.MediaPlayer2.skratch",
        "--object-path",
        "/org/mpris/MediaPlayer2",
        "--method",
        "org.mpris.MediaPlayer2.Player.SetPosition",
        "/",
        "7500000",
    ]);
    assert_eq!(receive_media_command(&controls), MediaCommand::SeekTo(7.5));

    gdbus_call(&[
        "call",
        "--session",
        "--dest",
        "org.mpris.MediaPlayer2.skratch",
        "--object-path",
        "/org/mpris/MediaPlayer2",
        "--method",
        "org.freedesktop.DBus.Properties.Set",
        "org.mpris.MediaPlayer2.Player",
        "Volume",
        "<double 0.42>",
    ]);
    assert!(matches!(
        receive_media_command(&controls),
        MediaCommand::SetVolume(volume) if (volume - 42.0).abs() < 0.001
    ));
}
