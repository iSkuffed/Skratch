use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender, TrySendError};
use std::thread;
use std::time::{Duration, Instant};
use tempfile::TempDir;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const COMMAND_TIMEOUT: Duration = Duration::from_secs(2);
const POSITION_EVENT_INTERVAL: f64 = 0.25;

#[derive(Debug, Clone, PartialEq)]
pub enum PlayerEvent {
    Started,
    Position(f64),
    Duration(f64),
    Paused(bool),
    Volume(f64),
    Ended,
    Error(String),
}

type CommandReply = (u64, std::result::Result<(), String>);

pub struct Player {
    pub events: Receiver<PlayerEvent>,
    stream: UnixStream,
    replies: Receiver<CommandReply>,
    child: Option<Child>,
    _socket_dir: TempDir,
    next_request_id: u64,
    shutdown: Arc<AtomicBool>,
}

impl Player {
    pub fn start(wake: std::sync::Arc<dyn Fn() + Send + Sync>) -> Result<Self> {
        Self::start_with_output(wake, None)
    }

    pub fn start_with_output(
        wake: std::sync::Arc<dyn Fn() + Send + Sync>,
        output: Option<&str>,
    ) -> Result<Self> {
        if output.is_some_and(|output| output != "null") {
            bail!("the only supported explicit mpv output is `null`");
        }

        let socket_dir = tempfile::Builder::new()
            .prefix("skratch-mpv-")
            .tempdir()
            .context("create private mpv IPC directory")?;
        let socket_path = socket_dir.path().join("ipc.sock");
        let socket_arg = socket_path
            .to_str()
            .ok_or_else(|| anyhow!("mpv IPC socket path is not valid UTF-8"))?;
        if socket_arg.len() >= 108 {
            bail!("mpv IPC socket path is too long for a Unix socket");
        }

        let mut command = Command::new("mpv");
        command
            .arg("--idle=yes")
            .arg("--no-video")
            .arg("--no-terminal")
            .arg("--no-config")
            // Local audio needs small buffers, not mpv's 150 MiB forward cache.
            .arg("--demuxer-max-bytes=8MiB")
            .arg("--demuxer-max-back-bytes=1MiB")
            .arg("--pause=no")
            .arg("--load-scripts=no")
            .arg("--autoload-files=no")
            .arg(format!("--input-ipc-server={socket_arg}"));
        if output == Some("null") {
            command.arg("--ao=null");
        }
        let mut child = command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("start mpv (make sure mpv is installed)")?;

        let socket = match wait_for_socket(&socket_path, &mut child) {
            Ok(socket) => socket,
            Err(error) => {
                kill_and_reap(child);
                return Err(error);
            }
        };
        let stream = match socket.try_clone() {
            Ok(stream) => stream,
            Err(error) => {
                kill_and_reap(child);
                return Err(error).context("clone mpv IPC socket");
            }
        };
        if let Err(error) = stream.set_write_timeout(Some(COMMAND_TIMEOUT)) {
            kill_and_reap(child);
            return Err(error).context("set mpv IPC write timeout");
        }

        let (event_tx, events) = mpsc::sync_channel(64);
        let (reply_tx, replies) = mpsc::channel();
        let reader_wake = wake.clone();
        let shutdown = Arc::new(AtomicBool::new(false));
        let reader_shutdown = shutdown.clone();
        let reader = match thread::Builder::new()
            .name("skratch-mpv-ipc".to_owned())
            .spawn(move || read_messages(socket, event_tx, reply_tx, reader_wake, reader_shutdown))
        {
            Ok(reader) => reader,
            Err(error) => {
                kill_and_reap(child);
                return Err(error).context("start mpv IPC reader");
            }
        };
        drop(reader);

        let mut player = Self {
            events,
            stream,
            replies,
            child: Some(child),
            _socket_dir: socket_dir,
            next_request_id: 1,
            shutdown,
        };
        for (id, property) in [
            (1, "time-pos"),
            (2, "duration"),
            (3, "pause"),
            (4, "volume"),
        ] {
            player
                .command(vec![json!("observe_property"), json!(id), json!(property)])
                .with_context(|| format!("observe mpv property {property}"))?;
        }
        Ok(player)
    }

    pub fn load(&mut self, path: &Path) -> Result<()> {
        if !path.is_file() {
            bail!("media path is not a file: {}", path.display());
        }
        let path = path
            .canonicalize()
            .with_context(|| format!("resolve media path {}", path.display()))?;
        let path = path
            .to_str()
            .ok_or_else(|| anyhow!("media path is not valid UTF-8: {}", path.display()))?;
        self.command(vec![json!("loadfile"), json!(path), json!("replace")])
            .with_context(|| format!("load media file {path}"))?;
        self.set_paused(false)
    }

    pub fn set_paused(&mut self, paused: bool) -> Result<()> {
        self.command(vec![json!("set_property"), json!("pause"), json!(paused)])
            .context("set mpv pause state")
    }

    pub fn seek(&mut self, seconds: f64) -> Result<()> {
        if !seconds.is_finite() || seconds < 0.0 {
            bail!("seek position must be a finite non-negative number");
        }
        self.command(vec![json!("seek"), json!(seconds), json!("absolute+exact")])
            .context("seek in mpv")
    }

    pub fn set_volume(&mut self, volume: f64) -> Result<()> {
        if !volume.is_finite() || !(0.0..=100.0).contains(&volume) {
            bail!("volume must be between 0 and 100");
        }
        self.command(vec![json!("set_property"), json!("volume"), json!(volume)])
            .context("set mpv volume")
    }

    pub fn stop(&mut self) -> Result<()> {
        self.command(vec![json!("stop")])
            .context("stop mpv playback")
    }

    fn command(&mut self, arguments: Vec<Value>) -> Result<()> {
        let request_id = self.next_request_id;
        self.next_request_id = self.next_request_id.wrapping_add(1).max(1);
        let command_name = arguments
            .first()
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_owned();
        let request = json!({ "command": arguments, "request_id": request_id });
        serde_json::to_writer(&mut self.stream, &request).context("write mpv IPC command")?;
        self.stream
            .write_all(b"\n")
            .context("finish mpv IPC command")?;
        self.stream.flush().context("flush mpv IPC command")?;

        let deadline = Instant::now() + COMMAND_TIMEOUT;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                bail!("timed out waiting for mpv command `{command_name}`");
            }
            match self.replies.recv_timeout(remaining) {
                Ok((reply_id, result)) if reply_id == request_id => {
                    return result
                        .map_err(|error| anyhow!("mpv command `{command_name}` failed: {error}"));
                }
                Ok(_) => continue,
                Err(RecvTimeoutError::Timeout) => {
                    bail!("timed out waiting for mpv command `{command_name}`");
                }
                Err(RecvTimeoutError::Disconnected) => {
                    bail!("mpv IPC closed before command `{command_name}` completed");
                }
            }
        }
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        let _ = self.stream.shutdown(Shutdown::Both);
        if let Some(child) = self.child.take() {
            kill_and_reap(child);
        }
    }
}

fn wait_for_socket(path: &Path, child: &mut Child) -> Result<UnixStream> {
    let deadline = Instant::now() + CONNECT_TIMEOUT;
    loop {
        match UnixStream::connect(path) {
            Ok(socket) => return Ok(socket),
            Err(error) => {
                if let Some(status) = child.try_wait().context("check mpv startup status")? {
                    bail!("mpv exited before opening its IPC socket ({status})");
                }
                if Instant::now() >= deadline {
                    return Err(anyhow!("timed out waiting for mpv IPC socket: {error}"));
                }
                thread::sleep(Duration::from_millis(20));
            }
        }
    }
}

fn kill_and_reap(mut child: Child) {
    let _ = child.kill();
    let deadline = Instant::now() + Duration::from_millis(250);
    while Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) => thread::sleep(Duration::from_millis(10)),
            Err(_) => break,
        }
    }
    let _ = thread::Builder::new()
        .name("skratch-mpv-reaper".to_owned())
        .spawn(move || {
            let _ = child.wait();
        });
}

fn read_messages(
    socket: UnixStream,
    events: SyncSender<PlayerEvent>,
    replies: Sender<CommandReply>,
    wake: Arc<dyn Fn() + Send + Sync>,
    shutdown: Arc<AtomicBool>,
) {
    let mut reader = BufReader::new(socket);
    let mut last_position = None;
    let mut last_duration = None;
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => {
                emit(
                    &events,
                    &wake,
                    &shutdown,
                    PlayerEvent::Error("mpv IPC connection closed".into()),
                );
                break;
            }
            Ok(_) => {}
            Err(error) => {
                emit(
                    &events,
                    &wake,
                    &shutdown,
                    PlayerEvent::Error(format!("read mpv IPC response: {error}")),
                );
                break;
            }
        }

        let message: Value = match serde_json::from_str(&line) {
            Ok(message) => message,
            Err(error) => {
                emit(
                    &events,
                    &wake,
                    &shutdown,
                    PlayerEvent::Error(format!("invalid mpv IPC JSON: {error}")),
                );
                continue;
            }
        };

        if let (Some(request_id), Some(error)) = (
            message.get("request_id").and_then(Value::as_u64),
            message.get("error"),
        ) {
            if error.as_str() == Some("success") {
                let _ = replies.send((request_id, Ok(())));
            } else {
                let error = error
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| error.to_string());
                let _ = replies.send((request_id, Err(error)));
            }
        }

        match message.get("event").and_then(Value::as_str) {
            Some("property-change") => {
                let id = message.get("id").and_then(Value::as_i64);
                let value = message.get("data");
                match id {
                    Some(1) => {
                        if let Some(position) = value.and_then(Value::as_f64) {
                            let changed = last_position.is_none_or(|last: f64| {
                                (position - last).abs() >= POSITION_EVENT_INTERVAL
                                    || position < last - 0.05
                            });
                            if changed {
                                last_position = Some(position);
                                emit(&events, &wake, &shutdown, PlayerEvent::Position(position));
                            }
                        }
                    }
                    Some(2) => {
                        if let Some(duration) = value.and_then(Value::as_f64)
                            && last_duration != Some(duration)
                        {
                            last_duration = Some(duration);
                            emit(&events, &wake, &shutdown, PlayerEvent::Duration(duration));
                        }
                    }
                    Some(3) => {
                        if let Some(paused) = value.and_then(Value::as_bool) {
                            emit(&events, &wake, &shutdown, PlayerEvent::Paused(paused));
                        }
                    }
                    Some(4) => {
                        if let Some(volume) = value.and_then(Value::as_f64) {
                            emit(&events, &wake, &shutdown, PlayerEvent::Volume(volume));
                        }
                    }
                    _ => {}
                }
            }
            Some("file-loaded") => {
                last_position = None;
                last_duration = None;
                emit(&events, &wake, &shutdown, PlayerEvent::Started);
            }
            Some("end-file") => match message.get("reason").and_then(Value::as_str) {
                Some("eof") => emit(&events, &wake, &shutdown, PlayerEvent::Ended),
                Some("error") => {
                    let details = message
                        .get("file_error")
                        .and_then(Value::as_str)
                        .filter(|details| !details.is_empty())
                        .unwrap_or("mpv could not play the file");
                    emit(
                        &events,
                        &wake,
                        &shutdown,
                        PlayerEvent::Error(format!("mpv playback error: {details}")),
                    );
                }
                _ => {}
            },
            _ => {}
        }
    }
}

fn emit(
    events: &SyncSender<PlayerEvent>,
    wake: &Arc<dyn Fn() + Send + Sync>,
    shutdown: &AtomicBool,
    event: PlayerEvent,
) {
    if shutdown.load(Ordering::Acquire) {
        return;
    }
    match event {
        terminal @ (PlayerEvent::Started | PlayerEvent::Ended | PlayerEvent::Error(_)) => {
            wake();
            let mut pending = terminal;
            loop {
                match events.try_send(pending) {
                    Ok(()) => {
                        wake();
                        break;
                    }
                    Err(TrySendError::Disconnected(_)) => break,
                    Err(TrySendError::Full(event)) => {
                        if shutdown.load(Ordering::Acquire) {
                            break;
                        }
                        pending = event;
                        thread::sleep(Duration::from_millis(10));
                    }
                }
            }
        }
        telemetry => match events.try_send(telemetry) {
            Ok(()) => wake(),
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {}
        },
    }
}
