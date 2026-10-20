//! Verify the real yt-dlp/ffmpeg boundary with locally generated audio only.
use skratch::{
    download::{DownloadEvent, DownloadFormat, DownloadRequest, Downloader},
    library::Library,
};
use std::{
    io::{Read, Write},
    net::TcpListener,
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

#[test]
fn real_yt_dlp_downloads_local_audio_and_library_indexes_it() {
    if ["yt-dlp", "ffmpeg"]
        .iter()
        .any(|tool| Command::new(tool).arg("--version").output().is_err())
    {
        eprintln!("SKIPPED real downloader check: install yt-dlp and ffmpeg");
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("fixture.m4a");
    let output = Command::new("ffmpeg")
        .args([
            "-nostdin",
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
            "anullsrc=r=48000:cl=mono",
            "-t",
            "1",
            "-c:a",
            "aac",
        ])
        .arg(&source)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let bytes = std::fs::read(&source).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let done = Arc::new(AtomicBool::new(false));
    let server_done = done.clone();
    let server = std::thread::spawn(move || {
        while !server_done.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    stream
                        .set_read_timeout(Some(Duration::from_secs(1)))
                        .unwrap();
                    let mut request = [0_u8; 8192];
                    let count = stream.read(&mut request).unwrap_or(0);
                    let head = request[..count].starts_with(b"HEAD ");
                    let headers = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: audio/mp4\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        bytes.len()
                    );
                    let _ = stream.write_all(headers.as_bytes());
                    if !head {
                        let _ = stream.write_all(&bytes);
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(10))
                }
                Err(error) => panic!("test HTTP server: {error}"),
            }
        }
    });
    struct Stop(Arc<AtomicBool>);
    impl Drop for Stop {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Relaxed);
        }
    }
    let _stop = Stop(done.clone());
    let music = temp.path().join("music");
    std::fs::create_dir(&music).unwrap();
    let mut downloader = Downloader::new(Arc::new(|| {}));
    for format in [DownloadFormat::Best, DownloadFormat::Mp3] {
        downloader
            .start(DownloadRequest {
                url: format!("http://{address}/fixture.m4a"),
                destination: music.clone(),
                format,
            })
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let event = downloader
                .events
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .expect("download completion");
            match event {
                DownloadEvent::Finished(path) => {
                    assert!(path.is_file());
                    assert_eq!(path.parent(), Some(music.as_path()));
                    assert_eq!(
                        path.extension().and_then(|ext| ext.to_str()),
                        Some(if format == DownloadFormat::Mp3 {
                            "mp3"
                        } else {
                            "m4a"
                        })
                    );
                    break;
                }
                DownloadEvent::Failed(error) => panic!("actual yt-dlp failed: {error}"),
                DownloadEvent::Cancelled => panic!("unexpected cancellation"),
                DownloadEvent::Log(line) => eprintln!("{line}"),
            }
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        while downloader.is_running() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!downloader.is_running());
    }
    let mut library = Library::open(&temp.path().join("data")).unwrap();
    library.add_source(&music).unwrap();
    library.scan(false).unwrap();
    let tracks = library.tracks().unwrap();
    assert_eq!(tracks.len(), 2);
    assert!(tracks.iter().all(|track| track.duration > 0.9));
    assert!(!std::fs::read_dir(&music).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".skratch-download-")
    }));
    done.store(true, Ordering::Relaxed);
    server.join().unwrap();
}
