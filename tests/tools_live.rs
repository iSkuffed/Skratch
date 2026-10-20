//! Real downloads (~110 MB). Run with:
//! `cargo test --test tools_live -- --ignored --nocapture`
use skratch::tools::{self, Msg, Step};
use std::{process::Command, sync::mpsc};

#[test]
#[ignore = "downloads yt-dlp and ffmpeg from GitHub"]
fn yt_dlp_and_ffmpeg_install_into_the_data_dir_and_run() {
    let dir = tempfile::tempdir().unwrap();
    let (tx, rx) = mpsc::channel();
    tools::run(
        dir.path().to_owned(),
        vec![Step::YtDlp, Step::Ffmpeg, Step::UpdateYtDlp],
        tx,
        std::sync::Arc::new(|| {}),
    );
    let mut failures = Vec::new();
    for msg in rx {
        match msg {
            Msg::Log(line) => println!("{line}"),
            Msg::Done(step) => println!("done {step:?}"),
            Msg::Failed(step, error) => failures.push(format!("{step:?}: {error}")),
            Msg::Finished => break,
        }
    }
    assert!(failures.is_empty(), "{failures:?}");
    for (program, arg) in [
        (dir.path().join("bin/yt-dlp"), "--version"),
        (dir.path().join("ffmpeg/bin/ffmpeg"), "-version"),
        (dir.path().join("ffmpeg/bin/ffprobe"), "-version"),
    ] {
        let output = Command::new(&program).arg(arg).output().unwrap();
        assert!(output.status.success(), "{}", program.display());
    }
    assert!(tools::detect(dir.path()).downloads_ready());
}
