# Skratch

A small, native music player and library for Linux. Your files stay where they are, the app is one Rust binary (no webview), and playback is handled by `mpv`.

Skratch has a vinyl theme: a spinning record with a tonearm in the player bar, generated sleeves for tracks without cover art, and an accent colour taken from whatever cover is playing.

## Features

- **Library:** album, artist and track views. It scans your folders recursively, watches them for changes, and never copies or moves your music.
- **Formats:** MP3, FLAC, WAV, Ogg/Vorbis, Opus, AAC, M4A, MP4, AIFF, WMA, APE and WavPack. What actually plays depends on your mpv.
- **Search:** `Ctrl+F` jumps to the search box. Sort by album, artist, title, year or filename.
- **Tag editing:** edits go into the file itself, using a temp copy and an atomic rename so a failed write can't corrupt a track. Renaming never overwrites an existing file.
- **Batch tagging from filenames:** select several tracks and fill their tags from names like `Artist - Title ft. Someone`. The pattern is configurable (`{artist} {title} {album} {track}`). Featured artists are detected automatically.
- **Cover search:** "Find cover online..." (track menu, album page, edit dialog, or a multi-selection) looks up covers on iTunes and MusicBrainz / Cover Art Archive. It only searches when you ask. By default the cover is kept in Skratch's own cache, survives rescans and never touches your audio files. Tick "Also save into the audio file" to embed it too, so other players show it; the choice is remembered. Double-click a cover to use it. With several albums selected, or "Find covers for the N tracks without one..." on the Albums page, it steps through them one at a time: pick a cover and the next album comes up.
- **Downloads:** paste a link and Skratch runs `yt-dlp` + `ffmpeg` in the background, with progress and cancel.
- **Playback:** queue, shuffle, repeat, seeking, media keys and the desktop now-playing widget (MPRIS). Opening the app never starts audio on its own.
- **Controls:** `Space` toggles play/pause (unless you're typing). Scroll over the volume slider to change it. A single click plays a track; there's a setting to switch to double-click.

## Install

Build from source (Rust toolchain, a C compiler, `pkg-config` and D-Bus headers are required; on Fedora that's `gcc pkgconf-pkg-config dbus-devel`, on Debian/Ubuntu `build-essential pkg-config libdbus-1-dev`):

```sh
git clone https://github.com/iSkuffed/Skratch
cd Skratch
cargo build --release
./target/release/skratch
```

On first launch a welcome page checks what you already have and sets up the rest:

- **Music folder:** pick where your music lives.
- **mpv** (plays the audio): if it's missing, "Install mpv" runs your package manager (dnf, apt, pacman or zypper) through the desktop's password prompt, so Skratch never sees your password. The exact command is shown too, if you'd rather run it yourself.
- **Downloads (optional):** one checkbox fetches `yt-dlp` (about 40 MB) and `ffmpeg` (about 70 MB, 220 MB unpacked) from their official releases, checks them against the published checksums and keeps them in Skratch's own folder. Nothing is installed system-wide, and tools already on your system are used as they are.

You can reopen it any time with "Run setup again" in the sidebar.

Prebuilt downloads aren't published yet.

## Where things live

The library database, cached artwork, player state and any downloaded tools (`bin/`, `ffmpeg/`) are in `~/.local/share/skratch`. Set `SKRATCH_DATA_DIR=/some/path` to use a separate library, which is handy for testing.

## Development

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
# MPRIS tests against an isolated D-Bus session:
dbus-run-session -- cargo test --test playback -- --test-threads=1
```

Tests use mpv's null audio output and generated temporary media, never your music folders.

| File | What it does |
| --- | --- |
| `src/app.rs` | The egui interface |
| `src/style.rs` | Theme, title bar, record, tonearm and seek bar drawing |
| `src/artwork.rs` | Background cover loading, texture cache, accent colour |
| `src/library.rs` | SQLite index, tags, artwork cache, file edits |
| `src/tagparse.rs` | Filename pattern parser for batch tagging |
| `src/online.rs` | Cover search (iTunes, MusicBrainz, Cover Art Archive) |
| `src/player.rs` | Headless mpv child process and JSON IPC |
| `src/download.rs` | yt-dlp jobs, progress, cancellation |
| `src/tools.rs` | Finds and installs mpv, yt-dlp and ffmpeg (setup page) |
| `src/service.rs` | Background scans and folder watching |
| `src/media.rs`, `src/queue.rs` | MPRIS, playback order |

[RoadMap.html](RoadMap.html) lists what's done and what's planned; it works offline.

## License

GPL-3.0-or-later. See [LICENSE](LICENSE). AIMP was an inspiration; no AIMP code, skins or artwork are included.

Made by [iSkuffed](https://github.com/iSkuffed).
