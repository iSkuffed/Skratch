//! External tools Skratch needs: mpv (installed through the system package
//! manager) and yt-dlp + ffmpeg (downloaded into the data directory).
//! Everything runs on a background thread and only ever starts from a button.
use anyhow::{Context, Result, bail, ensure};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, mpsc::Sender},
    thread,
    time::Duration,
};

const YT_DLP_BASE: &str = "https://github.com/yt-dlp/yt-dlp/releases/latest/download";
const FFMPEG_BASE: &str = "https://github.com/BtbN/FFmpeg-Builds/releases/download/latest";
const MAX_TOOL_BYTES: u64 = 300 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Mpv,
    YtDlp,
    Ffmpeg,
    UpdateYtDlp,
}

pub enum Msg {
    Log(String),
    Done(Step),
    Failed(Step, String),
    /// Every requested step has been tried.
    Finished,
}

/// What is installed right now.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Status {
    pub mpv: bool,
    /// The yt-dlp to run, from PATH or from our own copy.
    pub yt_dlp: Option<PathBuf>,
    /// True when `yt_dlp` is the copy Skratch downloaded (the only one it updates).
    pub yt_dlp_managed: bool,
    pub ffmpeg: bool,
    /// Set only for our own ffmpeg copy; a system ffmpeg is found by yt-dlp itself.
    pub ffmpeg_dir: Option<PathBuf>,
}

impl Status {
    pub fn downloads_ready(&self) -> bool {
        self.yt_dlp.is_some() && self.ffmpeg
    }
}

pub fn on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

fn managed_yt_dlp(data_dir: &Path) -> PathBuf {
    data_dir.join("bin").join("yt-dlp")
}

fn managed_ffmpeg(data_dir: &Path) -> PathBuf {
    data_dir.join("ffmpeg")
}

pub fn detect(data_dir: &Path) -> Status {
    let mut status = Status {
        mpv: on_path("mpv").is_some(),
        ..Status::default()
    };
    let own = managed_yt_dlp(data_dir);
    if let Some(system) = on_path("yt-dlp") {
        status.yt_dlp = Some(system);
    } else if own.is_file() {
        status.yt_dlp = Some(own);
        status.yt_dlp_managed = true;
    }
    let bin = managed_ffmpeg(data_dir).join("bin");
    if on_path("ffmpeg").is_some() && on_path("ffprobe").is_some() {
        status.ffmpeg = true;
    } else if bin.join("ffmpeg").is_file() && bin.join("ffprobe").is_file() {
        status.ffmpeg = true;
        status.ffmpeg_dir = Some(bin);
    }
    status
}

/// The package-manager command that installs mpv, if we know this system's.
pub fn mpv_command(has: &dyn Fn(&str) -> bool) -> Option<Vec<String>> {
    let words: &[&str] = if has("dnf") {
        &["dnf", "install", "-y", "mpv"]
    } else if has("apt-get") {
        &["apt-get", "install", "-y", "mpv"]
    } else if has("pacman") {
        &["pacman", "-S", "--noconfirm", "mpv"]
    } else if has("zypper") {
        &["zypper", "--non-interactive", "install", "mpv"]
    } else {
        return None;
    };
    Some(words.iter().map(|word| (*word).to_owned()).collect())
}

/// The mpv install command for this machine, as a line the user can paste.
pub fn mpv_shell_line() -> Option<String> {
    mpv_command(&|name| on_path(name).is_some()).map(|words| format!("sudo {}", words.join(" ")))
}

pub fn can_ask_for_password() -> bool {
    on_path("pkexec").is_some()
}

/// Starts the steps in order on a background thread.
pub fn run(
    data_dir: PathBuf,
    steps: Vec<Step>,
    tx: Sender<Msg>,
    wake: Arc<dyn Fn() + Send + Sync>,
) {
    thread::spawn(move || {
        let send = |msg: Msg| {
            let _ = tx.send(msg);
            wake();
        };
        for step in steps {
            let result = {
                let mut log = |line: String| send(Msg::Log(line));
                match step {
                    Step::Mpv => install_mpv(&mut log),
                    Step::YtDlp => install_yt_dlp(&data_dir, &mut log),
                    Step::Ffmpeg => install_ffmpeg(&data_dir, &mut log),
                    Step::UpdateYtDlp => update_yt_dlp(&data_dir, &mut log),
                }
            };
            match result {
                Ok(()) => send(Msg::Done(step)),
                Err(error) => send(Msg::Failed(step, format!("{error:#}"))),
            }
        }
        send(Msg::Finished);
    });
}

fn install_mpv(log: &mut dyn FnMut(String)) -> Result<()> {
    let words = mpv_command(&|name| on_path(name).is_some())
        .context("no known package manager (dnf, apt, pacman or zypper) found")?;
    ensure!(can_ask_for_password(), "pkexec is not available");
    log(format!("Asking for permission to run: {}", words.join(" ")));
    let (reader, writer) = std::io::pipe()?;
    let mut child = Command::new("pkexec")
        .args(&words)
        .stdin(Stdio::null())
        .stdout(writer.try_clone()?)
        .stderr(writer)
        .spawn()
        .context("start pkexec")?;
    // dnf redraws progress with carriage returns; show only the latest piece.
    for line in BufReader::new(reader).split(b'\n').map_while(Result::ok) {
        let text = String::from_utf8_lossy(&line);
        let text = text.rsplit('\r').find(|part| !part.trim().is_empty());
        if let Some(text) = text {
            log(text.trim().to_owned());
        }
    }
    let status = child.wait()?;
    match status.code() {
        Some(0) => {}
        Some(126 | 127) => bail!("the password prompt was cancelled or not allowed"),
        _ => bail!("the package manager failed ({status})"),
    }
    ensure!(on_path("mpv").is_some(), "mpv still isn't on PATH");
    Ok(())
}

fn asset_names() -> Result<(&'static str, &'static str)> {
    match std::env::consts::ARCH {
        "x86_64" => Ok((
            "yt-dlp_linux",
            "ffmpeg-master-latest-linux64-gpl-shared.tar.xz",
        )),
        "aarch64" => Ok((
            "yt-dlp_linux_aarch64",
            "ffmpeg-master-latest-linuxarm64-gpl-shared.tar.xz",
        )),
        other => bail!("no prebuilt downloads for {other}"),
    }
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .user_agent(concat!(
            "Skratch/",
            env!("CARGO_PKG_VERSION"),
            " ( https://github.com/iSkuffed )"
        ))
        .https_only(true)
        .timeout_connect(Some(Duration::from_secs(20)))
        .build()
        .into()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Looks `name` up in a `sha256sum`-style listing ("<hash>  <name>").
fn expected_hash(sums: &str, name: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let (hash, file) = line.split_once(char::is_whitespace)?;
        (file.trim().trim_start_matches('*') == name).then(|| hash.to_ascii_lowercase())
    })
}

fn fetch_sums(base: &str, file: &str, name: &str) -> Result<String> {
    let text = agent()
        .get(&format!("{base}/{file}"))
        .call()?
        .body_mut()
        .with_config()
        .limit(1024 * 1024)
        .read_to_string()?;
    expected_hash(&text, name).with_context(|| format!("no checksum listed for {name}"))
}

/// Streams `url` into `dest`, returning the SHA-256 of what was written.
fn download(url: &str, dest: &Path, label: &str, log: &mut dyn FnMut(String)) -> Result<String> {
    let response = agent().get(url).call()?;
    let total = response.body().content_length();
    let mut reader = response
        .into_body()
        .into_with_config()
        .limit(MAX_TOOL_BYTES)
        .reader();
    let mut file = fs::File::create(dest).with_context(|| format!("create {}", dest.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 64 * 1024];
    let (mut done, mut shown) = (0u64, 0u64);
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        file.write_all(&buffer[..read])?;
        done += read as u64;
        if done - shown >= 2 * 1024 * 1024 {
            shown = done;
            let of = total.map(|t| format!(" of {} MB", t / 1_000_000));
            log(format!(
                "Downloading {label}: {} MB{}",
                done / 1_000_000,
                of.unwrap_or_default()
            ));
        }
    }
    file.sync_all()?;
    if let Some(total) = total {
        ensure!(done == total, "download of {label} ended early");
    }
    Ok(hex(&hasher.finalize()))
}

fn install_yt_dlp(data_dir: &Path, log: &mut dyn FnMut(String)) -> Result<()> {
    let (asset, _) = asset_names()?;
    let dir = data_dir.join("bin");
    fs::create_dir_all(&dir)?;
    let part = dir.join(".yt-dlp.part");
    let result = (|| {
        log("Checking the yt-dlp checksum...".to_owned());
        let want = fetch_sums(YT_DLP_BASE, "SHA2-256SUMS", asset)?;
        let got = download(&format!("{YT_DLP_BASE}/{asset}"), &part, "yt-dlp", log)?;
        ensure!(got == want, "yt-dlp download failed its checksum");
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&part, fs::Permissions::from_mode(0o755))?;
        fs::rename(&part, managed_yt_dlp(data_dir)).context("put yt-dlp in place")
    })();
    if result.is_err() {
        let _ = fs::remove_file(&part);
    }
    result
}

fn update_yt_dlp(data_dir: &Path, log: &mut dyn FnMut(String)) -> Result<()> {
    let program = managed_yt_dlp(data_dir);
    ensure!(
        program.is_file(),
        "Skratch has no yt-dlp copy of its own to update"
    );
    log("Updating yt-dlp...".to_owned());
    let output = Command::new(&program)
        .arg("-U")
        .stdin(Stdio::null())
        .output()
        .context("run yt-dlp -U")?;
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        log(line.to_owned());
    }
    ensure!(output.status.success(), "yt-dlp could not update itself");
    Ok(())
}

fn install_ffmpeg(data_dir: &Path, log: &mut dyn FnMut(String)) -> Result<()> {
    let (_, asset) = asset_names()?;
    fs::create_dir_all(data_dir)?;
    let archive = data_dir.join(".ffmpeg.part.tar.xz");
    let fresh = data_dir.join(".ffmpeg.new");
    let result = (|| {
        log("Checking the ffmpeg checksum...".to_owned());
        let want = fetch_sums(FFMPEG_BASE, "checksums.sha256", asset)?;
        let got = download(&format!("{FFMPEG_BASE}/{asset}"), &archive, "ffmpeg", log)?;
        ensure!(got == want, "ffmpeg download failed its checksum");
        log("Unpacking ffmpeg...".to_owned());
        let _ = fs::remove_dir_all(&fresh);
        extract_ffmpeg(&archive, &fresh)?;
        for tool in ["ffmpeg", "ffprobe"] {
            let ok = Command::new(fresh.join("bin").join(tool))
                .arg("-version")
                .stdin(Stdio::null())
                .output()
                .map(|output| output.status.success())
                .unwrap_or(false);
            ensure!(ok, "the downloaded {tool} does not run on this system");
        }
        let target = managed_ffmpeg(data_dir);
        let _ = fs::remove_dir_all(&target);
        fs::rename(&fresh, &target).context("put ffmpeg in place")
    })();
    let _ = fs::remove_file(&archive);
    if result.is_err() {
        let _ = fs::remove_dir_all(&fresh);
    }
    result
}

/// Unpacks only `bin/` and `lib/` of a .tar.xz with one top-level directory.
fn extract_ffmpeg(archive: &Path, dest: &Path) -> Result<()> {
    let file = fs::File::open(archive)?;
    let mut tar = tar::Archive::new(xz2::read::XzDecoder::new(file));
    for entry in tar.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        let mut parts = Vec::new();
        for component in path.components() {
            match component {
                Component::Normal(part) => parts.push(part.to_owned()),
                Component::CurDir => {}
                _ => bail!("unsafe path in archive: {}", path.display()),
            }
        }
        // parts[0] is the top-level directory
        if parts.len() < 3 || !(parts[1] == "bin" || parts[1] == "lib") {
            continue;
        }
        let target: PathBuf = std::iter::once(dest.to_owned())
            .chain(parts[1..].iter().map(PathBuf::from))
            .collect();
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        entry.unpack(&target)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn tar_xz(
        build: impl FnOnce(&mut tar::Builder<xz2::write::XzEncoder<fs::File>>),
    ) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let file = fs::File::create(dir.path().join("a.tar.xz")).unwrap();
        let mut builder = tar::Builder::new(xz2::write::XzEncoder::new(file, 1));
        build(&mut builder);
        builder.into_inner().unwrap().finish().unwrap();
        dir
    }

    fn file_entry(
        builder: &mut tar::Builder<xz2::write::XzEncoder<fs::File>>,
        path: &str,
        body: &[u8],
    ) {
        let mut header = tar::Header::new_gnu();
        header.set_size(body.len() as u64);
        header.set_mode(0o755);
        builder.append_data(&mut header, path, body).unwrap();
    }

    #[test]
    fn checksum_listing_is_matched_by_name() {
        let sums = "AAA111  other.bin\nbbb222 *yt-dlp_linux\nccc333  yt-dlp_linux_aarch64\n";
        assert_eq!(
            expected_hash(sums, "yt-dlp_linux").as_deref(),
            Some("bbb222")
        );
        assert_eq!(expected_hash(sums, "other.bin").as_deref(), Some("aaa111"));
        assert_eq!(expected_hash(sums, "missing"), None);
    }

    #[test]
    fn mpv_command_follows_the_package_manager() {
        let only = |name: &'static str| move |c: &str| c == name;
        assert_eq!(mpv_command(&only("dnf")).unwrap()[0], "dnf");
        assert_eq!(mpv_command(&only("apt-get")).unwrap()[0], "apt-get");
        assert_eq!(mpv_command(&only("pacman")).unwrap()[0], "pacman");
        assert_eq!(mpv_command(&only("zypper")).unwrap()[0], "zypper");
        assert!(mpv_command(&|_| false).is_none());
    }

    #[test]
    fn ffmpeg_extraction_keeps_bin_and_lib_and_skips_the_rest() {
        let dir = tar_xz(|b| {
            file_entry(b, "top/bin/ffmpeg", b"#!/bin/sh\n");
            file_entry(b, "top/lib/libx.so.1", b"lib");
            let mut link = tar::Header::new_gnu();
            link.set_entry_type(tar::EntryType::Symlink);
            link.set_size(0);
            b.append_link(&mut link, "top/lib/libx.so", "libx.so.1")
                .unwrap();
            file_entry(b, "top/include/a.h", b"header");
            file_entry(b, "top/LICENSE.txt", b"license");
        });
        let out = dir.path().join("out");
        extract_ffmpeg(&dir.path().join("a.tar.xz"), &out).unwrap();
        let mode = fs::metadata(out.join("bin/ffmpeg"))
            .unwrap()
            .permissions()
            .mode();
        assert_ne!(mode & 0o111, 0, "ffmpeg must stay executable");
        assert_eq!(fs::read(out.join("lib/libx.so")).unwrap(), b"lib");
        assert!(!out.join("include").exists());
        assert!(!out.join("LICENSE.txt").exists());
    }

    #[test]
    fn ffmpeg_extraction_refuses_paths_that_escape() {
        let dir = tar_xz(|b| {
            let mut header = tar::Header::new_gnu();
            header.set_size(1);
            header.set_mode(0o644);
            let name = b"top/bin/../../evil";
            header.as_old_mut().name[..name.len()].copy_from_slice(name);
            header.set_cksum();
            b.append(&header, &b"x"[..]).unwrap();
        });
        let out = dir.path().join("out");
        assert!(extract_ffmpeg(&dir.path().join("a.tar.xz"), &out).is_err());
        assert!(!dir.path().join("evil").exists());
    }
}
