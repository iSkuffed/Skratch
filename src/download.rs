use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::{
    ffi::{OsStr, OsString},
    fs,
    io::{self, BufRead, BufReader, Read},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    thread::{self, JoinHandle},
};
use tempfile::{Builder, NamedTempFile, TempDir};
use url::Url;

const MAX_URL_BYTES: usize = 8 * 1024;
const MAX_LOG_LINE_BYTES: usize = 8 * 1024;
const LOG_CHANNEL_CAPACITY: usize = 128;
const MAX_ERROR_BYTES: usize = 2 * 1024;
const FINISHED_MARKER: &str = "__SKRATCH_FINISHED__";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum DownloadFormat {
    #[default]
    Best,
    Mp3,
}

#[derive(Debug, Clone)]
pub struct DownloadRequest {
    pub url: String,
    pub destination: PathBuf,
    pub format: DownloadFormat,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DownloadEvent {
    Log(String),
    Finished(PathBuf),
    Failed(String),
    Cancelled,
}

pub struct Downloader {
    pub events: Receiver<DownloadEvent>,
    sender: SyncSender<DownloadEvent>,
    wake: Arc<dyn Fn() + Send + Sync>,
    worker: Option<JoinHandle<()>>,
    control: Arc<JobControl>,
    running: Arc<AtomicBool>,
    program: PathBuf,
}

#[derive(Clone)]
struct EventSink {
    sender: SyncSender<DownloadEvent>,
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl EventSink {
    fn log(&self, message: String) {
        if self.sender.try_send(DownloadEvent::Log(message)).is_ok() {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (self.wake)()));
        }
    }

    fn send(&self, event: DownloadEvent) {
        if self.sender.send(event).is_ok() {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (self.wake)()));
        }
    }
}

impl Downloader {
    pub fn new(wake: Arc<dyn Fn() + Send + Sync>) -> Self {
        Self::with_program(wake, PathBuf::from("yt-dlp"))
    }

    fn with_program(wake: Arc<dyn Fn() + Send + Sync>, program: PathBuf) -> Self {
        let (sender, events) = mpsc::sync_channel(LOG_CHANNEL_CAPACITY);
        Self {
            events,
            sender,
            wake,
            worker: None,
            control: Arc::new(JobControl::default()),
            running: Arc::new(AtomicBool::new(false)),
            program,
        }
    }

    pub fn start(&mut self, request: DownloadRequest) -> Result<()> {
        ensure!(!self.is_running(), "a download is already running");
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }

        let (url, destination) = validate_request(&request)?;
        let stage = Builder::new()
            .prefix(".skratch-download-")
            .tempdir_in(&destination)
            .context("create temporary download directory")?;
        let control = Arc::new(JobControl::default());
        self.control = Arc::clone(&control);
        self.running.store(true, Ordering::Release);

        let events = EventSink {
            sender: self.sender.clone(),
            wake: Arc::clone(&self.wake),
        };
        let running = Arc::clone(&self.running);
        let program = self.program.clone();
        let worker = thread::Builder::new()
            .name("skratch-download".to_owned())
            .spawn(move || {
                let _running = RunningGuard(running);
                let result = run_download(
                    program,
                    request,
                    url,
                    destination,
                    stage,
                    control.clone(),
                    events.clone(),
                );
                let event = match result {
                    Ok(path) => DownloadEvent::Finished(path),
                    Err(_error) if control.is_cancelled() => DownloadEvent::Cancelled,
                    Err(error) => {
                        DownloadEvent::Failed(bounded_text(format!("{error:#}"), MAX_ERROR_BYTES))
                    }
                };
                events.send(event);
            });
        let worker = match worker {
            Ok(worker) => worker,
            Err(error) => {
                self.running.store(false, Ordering::Release);
                return Err(error).context("start download worker");
            }
        };
        self.worker = Some(worker);
        Ok(())
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Acquire)
    }

    pub fn cancel(&self) {
        self.control.cancel();
    }
}

impl Drop for Downloader {
    fn drop(&mut self) {
        let (_sender, replacement) = mpsc::sync_channel(0);
        drop(std::mem::replace(&mut self.events, replacement));
        self.cancel();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

struct RunningGuard(Arc<AtomicBool>);

impl Drop for RunningGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

#[derive(Default)]
struct JobControl {
    state: Mutex<ControlState>,
}

#[derive(Default)]
struct ControlState {
    process_group: Option<i32>,
    cancelled: bool,
    committing: bool,
}

impl JobControl {
    fn cancel(&self) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if state.committing {
            return;
        }
        state.cancelled = true;
        if let Some(process_group) = state.process_group {
            kill_process_group(process_group);
        }
    }

    fn is_cancelled(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .cancelled
    }

    fn set_process_group(&self, process_group: i32) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.process_group = Some(process_group);
        if state.cancelled {
            kill_process_group(process_group);
        }
    }

    fn clear_process_group(&self) {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .process_group = None;
    }

    fn begin_commit(&self) -> bool {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if state.cancelled {
            return false;
        }
        state.committing = true;
        true
    }
}

fn validate_request(request: &DownloadRequest) -> Result<(Url, PathBuf)> {
    ensure!(!request.url.is_empty(), "URL is required");
    ensure!(request.url.len() <= MAX_URL_BYTES, "URL is too long");
    ensure!(
        !request.url.chars().any(char::is_control) && !request.url.chars().any(char::is_whitespace),
        "URL must not contain whitespace or control characters"
    );
    let url = Url::parse(&request.url).context("invalid URL")?;
    ensure!(
        matches!(url.scheme(), "http" | "https") && url.host_str().is_some(),
        "URL must use HTTP or HTTPS and include a host"
    );
    let authority = request
        .url
        .split_once("://")
        .map(|(_, rest)| rest.split(['/', '?', '#']).next().unwrap_or_default())
        .unwrap_or_default();
    ensure!(
        !authority.is_empty() && !authority.contains('\\'),
        "URL must include an explicit host after http:// or https://"
    );
    ensure!(
        !authority.contains('@') && url.username().is_empty() && url.password().is_none(),
        "URL credentials are not allowed"
    );
    ensure!(
        request.destination.is_absolute(),
        "destination must be an absolute existing directory"
    );
    let destination = fs::canonicalize(&request.destination)
        .with_context(|| format!("resolve destination {}", request.destination.display()))?;
    ensure!(
        destination.is_dir(),
        "destination must be an existing directory"
    );
    ensure!(
        destination.to_str().is_some(),
        "destination path must be valid UTF-8"
    );
    Ok((url, destination))
}

fn run_download(
    program: PathBuf,
    request: DownloadRequest,
    url: Url,
    destination: PathBuf,
    stage: TempDir,
    control: Arc<JobControl>,
    events: EventSink,
) -> Result<PathBuf> {
    if control.is_cancelled() {
        bail!("download cancelled");
    }
    let stage_path =
        fs::canonicalize(stage.path()).context("resolve temporary download directory")?;
    let args = command_args(&request, &url, &stage_path);
    let mut command = Command::new(&program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    configure_process_group(&mut command);

    let mut child = command
        .spawn()
        .with_context(|| format!("start {}", program.display()))?;
    let pid = i32::try_from(child.id()).context("yt-dlp process id is out of range")?;
    control.set_process_group(pid);
    if control.is_cancelled() {
        control.cancel();
    }

    let marker = Arc::new(Mutex::new(MarkerState::default()));
    let stdout = child.stdout.take().context("capture yt-dlp output")?;
    let stderr = child.stderr.take().context("capture yt-dlp errors")?;
    let stdout_reader = match spawn_reader(stdout, Arc::clone(&marker), events.clone()) {
        Ok(reader) => reader,
        Err(error) => {
            control.cancel();
            let _ = child.wait();
            control.clear_process_group();
            return Err(error).context("read yt-dlp output");
        }
    };
    let stderr_reader = match spawn_reader(stderr, Arc::clone(&marker), events) {
        Ok(reader) => reader,
        Err(error) => {
            control.cancel();
            let _ = child.wait();
            control.clear_process_group();
            let _ = stdout_reader.join();
            return Err(error).context("read yt-dlp output");
        }
    };

    let status = match child.wait() {
        Ok(status) => status,
        Err(error) => {
            control.cancel();
            control.clear_process_group();
            let _ = stdout_reader.join();
            let _ = stderr_reader.join();
            return Err(error).context("wait for yt-dlp");
        }
    };
    control.clear_process_group();
    stdout_reader
        .join()
        .map_err(|_| anyhow::anyhow!("yt-dlp output reader stopped unexpectedly"))??;
    stderr_reader
        .join()
        .map_err(|_| anyhow::anyhow!("yt-dlp error reader stopped unexpectedly"))??;

    if control.is_cancelled() {
        bail!("download cancelled");
    }
    ensure!(status.success(), "yt-dlp exited with {status}");

    let marker = marker.lock().unwrap_or_else(|error| error.into_inner());
    ensure!(!marker.invalid, "yt-dlp returned an invalid output path");
    ensure!(
        marker.count == 1,
        "yt-dlp did not produce exactly one audio file"
    );
    let output = marker
        .path
        .as_ref()
        .context("yt-dlp did not report its output file")?;
    let metadata = fs::symlink_metadata(output).context("read completed download")?;
    ensure!(
        metadata.file_type().is_file(),
        "download output is not a regular file"
    );
    let output = fs::canonicalize(output).context("resolve completed download")?;
    ensure!(
        output.starts_with(&stage_path),
        "download output escaped its temporary directory"
    );
    ensure!(
        playable_audio_path(&output),
        "download output format is not supported by the library"
    );
    let file_name = output
        .file_name()
        .context("download output has no filename")?;
    ensure!(
        file_name.to_str().is_some(),
        "download filename must be valid UTF-8"
    );
    ensure!(control.begin_commit(), "download cancelled");
    publish(&output, &destination, file_name)
}

fn command_args(request: &DownloadRequest, url: &Url, stage: &Path) -> Vec<OsString> {
    let mut args = vec![
        "--no-config".into(),
        "--no-plugin-dirs".into(),
        "--no-cache-dir".into(),
        "--no-playlist".into(),
        "--playlist-items".into(),
        "1".into(),
        "--socket-timeout".into(),
        "20".into(),
        "--retries".into(),
        "3".into(),
        "--fragment-retries".into(),
        "3".into(),
        "--extractor-retries".into(),
        "3".into(),
        "--file-access-retries".into(),
        "3".into(),
        "--newline".into(),
        "--no-colors".into(),
        "--progress".into(),
        "--format".into(),
        "bestaudio/best".into(),
        "--extract-audio".into(),
        "--audio-format".into(),
        match request.format {
            DownloadFormat::Best => "best".into(),
            DownloadFormat::Mp3 => "mp3".into(),
        },
    ];
    if request.format == DownloadFormat::Mp3 {
        args.extend(["--audio-quality".into(), "0".into()]);
    }
    args.extend([
        "--embed-metadata".into(),
        "--embed-thumbnail".into(),
        "--convert-thumbnails".into(),
        "jpg".into(),
        "--paths".into(),
        stage.as_os_str().to_owned(),
        "--output".into(),
        "%(id).80s.%(ext)s".into(),
        "--print".into(),
        format!("after_move:{FINISHED_MARKER}%(filepath)j").into(),
        "--no-simulate".into(),
        "--".into(),
        url.as_str().into(),
    ]);
    args
}

fn spawn_reader<R: Read + Send + 'static>(
    stream: R,
    marker: Arc<Mutex<MarkerState>>,
    events: EventSink,
) -> io::Result<JoinHandle<io::Result<()>>> {
    thread::Builder::new().spawn(move || {
        let mut reader = BufReader::new(stream);
        while let Some(bytes) = read_bounded_line(&mut reader)? {
            let line = bounded_line(&bytes);
            if line.starts_with(FINISHED_MARKER) {
                record_marker(&line, &marker);
            } else {
                events.log(line.trim_end_matches('\r').to_owned());
            }
        }
        Ok(())
    })
}

fn read_bounded_line(reader: &mut impl BufRead) -> io::Result<Option<Vec<u8>>> {
    let mut line = Vec::with_capacity(MAX_LOG_LINE_BYTES);
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok((!line.is_empty()).then_some(line));
        }
        let end = available
            .iter()
            .position(|byte| *byte == b'\n')
            .unwrap_or(available.len());
        let take = end.min(MAX_LOG_LINE_BYTES.saturating_sub(line.len()));
        line.extend_from_slice(&available[..take]);
        let has_newline = end < available.len();
        let consumed = if has_newline {
            end + 1
        } else {
            available.len()
        };
        reader.consume(consumed);
        if has_newline {
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            return Ok(Some(line));
        }
    }
}

fn bounded_line(bytes: &[u8]) -> String {
    let mut line = String::with_capacity(bytes.len());
    for character in String::from_utf8_lossy(bytes).chars() {
        if line.len() + character.len_utf8() > MAX_LOG_LINE_BYTES {
            break;
        }
        line.push(character);
    }
    line
}

#[derive(Default)]
struct MarkerState {
    count: usize,
    invalid: bool,
    path: Option<PathBuf>,
}

fn record_marker(line: &str, marker: &Mutex<MarkerState>) {
    let mut marker = marker.lock().unwrap_or_else(|error| error.into_inner());
    marker.count = marker.count.saturating_add(1);
    let Some(json) = line.strip_prefix(FINISHED_MARKER) else {
        marker.invalid = true;
        return;
    };
    match serde_json::from_str::<String>(json.trim()) {
        Ok(path) if !path.is_empty() => marker.path = Some(PathBuf::from(path)),
        _ => marker.invalid = true,
    }
}

fn publish(source: &Path, destination: &Path, file_name: &OsStr) -> Result<PathBuf> {
    let target = destination.join(file_name);
    match fs::hard_link(source, &target) {
        Ok(()) => Ok(target),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            bail!(
                "destination already contains {}",
                file_name.to_string_lossy()
            )
        }
        Err(_) => {
            let mut temporary = NamedTempFile::new_in(
                source
                    .parent()
                    .context("download has no staging directory")?,
            )
            .context("create temporary publish file")?;
            io::copy(
                &mut fs::File::open(source).context("open completed download")?,
                &mut temporary,
            )
            .context("copy completed download")?;
            temporary
                .as_file()
                .sync_all()
                .context("flush completed download")?;
            match temporary.persist_noclobber(&target) {
                Ok(_) => Ok(target),
                Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => {
                    bail!(
                        "destination already contains {}",
                        file_name.to_string_lossy()
                    )
                }
                Err(error) => Err(error.error).context("publish completed download"),
            }
        }
    }
}

fn playable_audio_path(path: &Path) -> bool {
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

fn bounded_text(mut text: String, max_bytes: usize) -> String {
    if text.len() > max_bytes {
        let mut end = max_bytes;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.push('…');
    }
    text
}

#[cfg(unix)]
fn configure_process_group(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
}

#[cfg(not(unix))]
fn configure_process_group(_command: &mut Command) {}

#[cfg(unix)]
fn kill_process_group(process_group: i32) {
    unsafe extern "C" {
        fn kill(pid: i32, signal: i32) -> i32;
    }
    const SIGKILL: i32 = 9;
    // The child is its process-group leader; killing the group also stops ffmpeg children.
    let _ = unsafe { kill(-process_group, SIGKILL) };
}

#[cfg(not(unix))]
fn kill_process_group(_process_group: i32) {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        os::unix::fs::PermissionsExt,
        time::{Duration, Instant},
    };
    use tempfile::tempdir;

    fn fake_program(body: &str) -> (TempDir, PathBuf) {
        let dir = tempdir().unwrap();
        let path = dir.path().join("fake-yt-dlp");
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        let mut permissions = fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&path, permissions).unwrap();
        (dir, path)
    }

    fn output_script(extra: &str) -> String {
        format!(
            r#"
for arg do printf 'ARG=%s\n' "$arg"; done
stage=''
while [ "$#" -gt 0 ]; do
  if [ "$1" = '--paths' ]; then stage="$2"; shift 2; else shift; fi
done
{extra}
printf 'fake audio bytes' > "$stage/fake.mp3"
printf '__SKRATCH_FINISHED__"%s"\n' "$stage/fake.mp3"
"#
        )
    }

    fn request(destination: &Path, url: &str) -> DownloadRequest {
        DownloadRequest {
            url: url.to_owned(),
            destination: destination.to_owned(),
            format: DownloadFormat::Mp3,
        }
    }

    fn terminal(downloader: &Downloader, timeout: Duration) -> DownloadEvent {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match downloader.events.recv_timeout(remaining).unwrap() {
                event @ DownloadEvent::Finished(_)
                | event @ DownloadEvent::Failed(_)
                | event @ DownloadEvent::Cancelled => return event,
                DownloadEvent::Log(_) => {}
            }
        }
    }

    fn stage_dir(destination: &Path) -> PathBuf {
        fs::read_dir(destination)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| {
                path.file_name()
                    .and_then(OsStr::to_str)
                    .is_some_and(|name| name.starts_with(".skratch-download-"))
            })
            .unwrap()
    }

    fn wait_for_pid_file(destination: &Path) -> u32 {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let pid_file = stage_dir(destination).join("child.pid");
            if let Ok(pid) = fs::read_to_string(pid_file)
                && let Ok(pid) = pid.trim().parse()
            {
                return pid;
            }
            assert!(Instant::now() < deadline, "fake child did not start");
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn process_running(pid: u32) -> bool {
        let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) else {
            return false;
        };
        let Some((_, fields)) = stat.rsplit_once(") ") else {
            return false;
        };
        !fields.starts_with('Z')
    }

    fn wait_for_process_exit(pid: u32) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while process_running(pid) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(!process_running(pid), "fake ffmpeg child is still running");
    }

    #[test]
    fn command_is_argument_based_and_uses_bounded_single_item_options() {
        let request = request(Path::new("/tmp"), "https://example.test/watch?v=one");
        let url = Url::parse(&request.url).unwrap();
        let args = command_args(&request, &url, Path::new("/tmp/.skratch-download-stage"));
        let args = args
            .iter()
            .map(|arg| arg.to_string_lossy())
            .collect::<Vec<_>>();
        for option in [
            "--no-config",
            "--no-plugin-dirs",
            "--no-cache-dir",
            "--no-playlist",
            "--playlist-items",
            "--socket-timeout",
            "--retries",
            "--fragment-retries",
            "--extractor-retries",
            "--file-access-retries",
            "--progress",
            "--embed-metadata",
            "--embed-thumbnail",
            "--no-simulate",
        ] {
            assert!(args.iter().any(|arg| arg == option), "missing {option}");
        }
        assert_eq!(args.last().unwrap(), "https://example.test/watch?v=one");
        assert_eq!(args[args.len() - 2], "--");
        assert!(args.contains(&"%(id).80s.%(ext)s".into()));
        assert!(
            args.iter()
                .any(|arg| arg.starts_with("after_move:__SKRATCH_FINISHED__"))
        );
    }

    #[test]
    fn validation_rejects_bad_urls_and_destinations_before_creating_a_stage() {
        let temp = tempdir().unwrap();
        let valid = temp.path().join("music");
        fs::create_dir(&valid).unwrap();
        for url in [
            "file:///tmp/song.mp3",
            "https:///missing-host",
            "https://user:pass@example.test/audio",
            "https://@example.test/audio",
            "https://example.test/audio\nhttps://other.test/audio",
            "https://example.test/audio with spaces",
        ] {
            assert!(
                validate_request(&request(&valid, url)).is_err(),
                "accepted {url:?}"
            );
        }
        assert!(
            validate_request(&request(
                &temp.path().join("missing"),
                "https://example.test/a"
            ))
            .is_err()
        );
        assert!(
            validate_request(&request(Path::new("relative"), "https://example.test/a")).is_err()
        );
        assert_eq!(fs::read_dir(&valid).unwrap().count(), 0);
    }

    #[test]
    fn successful_fake_download_keeps_url_as_one_argument_and_publishes_once() {
        let destination_root = tempdir().unwrap();
        let destination = destination_root.path().join("music");
        fs::create_dir(&destination).unwrap();
        let url = "https://example.test/watch?v=$(touch%20SHOULD_NOT_EXIST);x=1";
        let (fake_dir, program) = fake_program(&output_script(""));
        let mut downloader = Downloader::with_program(Arc::new(|| {}), program);
        downloader.start(request(&destination, url)).unwrap();
        let final_path = match terminal(&downloader, Duration::from_secs(5)) {
            DownloadEvent::Finished(path) => path,
            other => panic!("unexpected terminal event: {other:?}"),
        };
        assert!(final_path.is_absolute());
        assert_eq!(fs::read(&final_path).unwrap(), b"fake audio bytes");
        assert!(!destination_root.path().join("SHOULD_NOT_EXIST").exists());
        assert!(fs::read_dir(&destination).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".skratch-download-")
        }));
        let _ = fake_dir;
    }

    #[test]
    fn failures_and_existing_names_leave_user_bytes_unchanged() {
        let root = tempdir().unwrap();
        let destination = root.path().join("music");
        fs::create_dir(&destination).unwrap();
        fs::write(destination.join("fake.mp3"), b"keep me").unwrap();
        let (fake_dir, program) = fake_program(&output_script(""));
        let mut downloader = Downloader::with_program(Arc::new(|| {}), program);
        downloader
            .start(request(&destination, "https://example.test/audio"))
            .unwrap();
        match terminal(&downloader, Duration::from_secs(5)) {
            DownloadEvent::Failed(message) => assert!(message.contains("already contains")),
            other => panic!("unexpected terminal event: {other:?}"),
        }
        assert_eq!(fs::read(destination.join("fake.mp3")).unwrap(), b"keep me");
        assert!(!stage_dir_if_any(&destination));
        let _ = fake_dir;
    }

    fn stage_dir_if_any(destination: &Path) -> bool {
        fs::read_dir(destination).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".skratch-download-")
        })
    }

    #[test]
    fn tool_failure_is_reported_and_cleans_the_staging_directory() {
        let root = tempdir().unwrap();
        let destination = root.path().join("music");
        fs::create_dir(&destination).unwrap();
        let (fake_dir, program) = fake_program("echo tool-failure >&2\nexit 7");
        let mut downloader = Downloader::with_program(Arc::new(|| {}), program);
        downloader
            .start(request(&destination, "https://example.test/audio"))
            .unwrap();
        match terminal(&downloader, Duration::from_secs(5)) {
            DownloadEvent::Failed(message) => assert!(message.contains("exited with")),
            other => panic!("unexpected terminal event: {other:?}"),
        }
        assert!(!stage_dir_if_any(&destination));
        let _ = fake_dir;
    }

    #[test]
    fn log_lines_and_queued_progress_events_are_bounded() {
        let root = tempdir().unwrap();
        let destination = root.path().join("music");
        fs::create_dir(&destination).unwrap();
        let script = output_script(
            "i=0; while [ $i -lt 400 ]; do printf '%012000d\\n' 0; i=$((i + 1)); done",
        );
        let (fake_dir, program) = fake_program(&script);
        let mut downloader = Downloader::with_program(Arc::new(|| {}), program);
        downloader
            .start(request(&destination, "https://example.test/audio"))
            .unwrap();
        let final_path = match terminal(&downloader, Duration::from_secs(10)) {
            DownloadEvent::Finished(path) => path,
            other => panic!("unexpected terminal event: {other:?}"),
        };
        let mut count = 1;
        while let Ok(event) = downloader.events.try_recv() {
            if let DownloadEvent::Log(line) = event {
                assert!(line.len() <= MAX_LOG_LINE_BYTES);
            }
            count += 1;
        }
        assert!(count <= LOG_CHANNEL_CAPACITY + 1);
        assert!(final_path.exists());
        let _ = fake_dir;
    }

    #[test]
    fn a_second_job_is_rejected_and_cancel_kills_the_child_process_group() {
        let root = tempdir().unwrap();
        let destination = root.path().join("music");
        fs::create_dir(&destination).unwrap();
        let (fake_dir, program) = fake_program(
            "stage=''; while [ \"$#\" -gt 0 ]; do if [ \"$1\" = '--paths' ]; then stage=\"$2\"; shift 2; else shift; fi; done\nsleep 30 &\necho $! > \"$stage/child.pid\"\nwait",
        );
        let mut downloader = Downloader::with_program(Arc::new(|| {}), program);
        downloader
            .start(request(&destination, "https://example.test/audio"))
            .unwrap();
        let pid = wait_for_pid_file(&destination);
        assert!(downloader.is_running());
        assert!(
            downloader
                .start(request(&destination, "https://example.test/second"))
                .is_err()
        );
        downloader.cancel();
        assert_eq!(
            terminal(&downloader, Duration::from_secs(5)),
            DownloadEvent::Cancelled
        );
        wait_for_process_exit(pid);
        assert!(!stage_dir_if_any(&destination));
        let _ = fake_dir;
    }

    #[test]
    fn dropping_downloader_cancels_and_reaps_the_active_worker() {
        let root = tempdir().unwrap();
        let destination = root.path().join("music");
        fs::create_dir(&destination).unwrap();
        let (fake_dir, program) = fake_program(
            "stage=''; while [ \"$#\" -gt 0 ]; do if [ \"$1\" = '--paths' ]; then stage=\"$2\"; shift 2; else shift; fi; done\nsleep 30 &\necho $! > \"$stage/child.pid\"\nwait",
        );
        let mut downloader = Downloader::with_program(Arc::new(|| {}), program);
        downloader
            .start(request(&destination, "https://example.test/audio"))
            .unwrap();
        let pid = wait_for_pid_file(&destination);
        drop(downloader);
        wait_for_process_exit(pid);
        assert!(!stage_dir_if_any(&destination));
        let _ = fake_dir;
    }
}
