# Skratch

A small, native music player and library for Linux. Your files stay where they are, the app is one Rust binary (no webview), and playback is handled by `mpv`.

Skratch has a vinyl theme: a spinning record with a tonearm in the player bar, generated sleeves for tracks without cover art, and an accent colour taken from whatever cover is playing.

## Features

- **Library:** album, artist and track views. It scans your folders recursively, watches them for changes, and never copies or moves your music.
- **Formats:** MP3, FLAC, WAV, Ogg/Vorbis, Opus, AAC, M4A, MP4, AIFF, WMA, APE and WavPack. What actually plays depends on your mpv.
- **Search:** `Ctrl+F` jumps to the search box. Sort by album, artist, title, year or filename.
- **Tag editing:** edits go into the file itself, using a temp copy and an atomic rename so a failed write can't corrupt a track. Renaming never overwrites an existing file.
- **Batch tagging from filenames:** select several tracks and fill their tags from names like `Artist - Title ft. Someone`. The pattern is configurable (`{artist} {title} {album} {track}`). Featured artists are detected automatically, and downloader clutter such as `(Official Video)` or a trailing `[video id]` is dropped.
- **Cover search:** "Find cover online..." (track menu, album page, edit dialog, or a multi-selection) looks up covers on iTunes and MusicBrainz / Cover Art Archive. It only searches when you ask. By default the cover is kept in Skratch's own cache, survives rescans and never touches your audio files. Tick "Also save into the audio file" to embed it too, so other players show it; the choice is remembered. Double-click a cover to use it. With several albums selected, or "Find covers for the N tracks without one..." on the Albums page, it steps through them one at a time: pick a cover and the next album comes up.
- **Downloads:** paste a link and Skratch runs `yt-dlp` + `ffmpeg` in the background, with progress and cancel.
- **Playback:** queue, shuffle, repeat, seeking, media keys and the desktop now-playing widget (MPRIS). Opening the app never starts audio on its own.
- **Plays and ratings:** a song counts as played once you have heard half of it (or four minutes), and its play count and last-played time are kept in Skratch's own database, not in the file. Give songs one to five stars on the Now playing details or from any track menu (it works on a whole selection). Sort lists by last played, play count or rating, and filter them to songs not played yet, played, or rated.
- **Sleep timer:** the Sleep button in the player bar stops the music in 15 to 90 minutes, or at the end of the song, fading the sound out first.
- **Gapless, fades and loudness:** the next song is lined up ahead of time so albums play without a pause (a setting, on by default). "Fade between songs" dips each song out and the next in over 2 to 10 seconds; it does not overlap two songs. "Even out loudness" uses ReplayGain tags per song or per album, or a compressor that levels songs with no tags.
- **Tidy up:** a page that lists songs which are in the library twice (same title and artist once filler like "(Official Video)" is ignored, within a few seconds of the same length) and songs whose tags look unfinished (no artist, album, year or track number, filler or numbering in the title). It only reads; the batch tools (fix tags from filenames, fetch metadata, covers) work on whatever is listed.
- **Now playing:** click the record in the player bar, or pick "Now playing" in the sidebar, for a big spinning record with the track details, the cover faded in behind them, and a halo that pulses with the bass (mp3, aac/m4a, flac, ogg, wav; Opus is not decoded yet, so those stay still). Click the record to pause or resume. The right-hand side has a row of tabs: track details, lyrics, the visualizer, the queue ("Up next") and the whole library. Lyrics and Visualizer are one page, the visualizer with the lyrics over it; each tab slides its quick options out along the row (Lyrics: look up or try again, type, import, hide; Visualizer: bars, mirror, ring, off, full screen), pushing the tabs after it to the right and cutting them off before the gear. Opening one closes the other, and so does clicking it again or clicking elsewhere. Up next and Library share one "Search" bar, so you can pick a track without leaving the page, and the bar stays put when you switch between the two. The visualizer keeps running behind both lists on a dark backing that fades out toward the bottom of the list. Switching tabs fades the new view in from the top down, and nothing moves. Hover the cog at the top right and it pulls a drawer out over the panel by a small tab on the drawer's left edge. The drawer has a Visualizer section and a Lyrics section, each with its own toggles, and covers the row of tabs while it is out. It slides back, cog and all, once the pointer has been away for a moment. The left sidebar scrolls when the window is too short to show all of it.
- **Lyrics:** read from a `.lrc` file next to the track or from the file's own tags. Synced lyrics follow the song and a click on a line jumps there. If neither exists, Skratch can look them up on [LRCLIB](https://lrclib.net), a free community database. That is off until you tick "Look up lyrics online", and it sends the artist, title, album and length. A second tick box adds NetEase Cloud Music for tracks LRCLIB doesn't have; it gets the artist and title. Results are cached locally. A third tick box adds Genius as the last resort: it asks genius.com for the song's page and reads the words from it, so you get plain lyrics with no timing, and since Genius's terms do not allow automated use it stays off until you switch it on. Select songs or an album and choose "Find lyrics" to look them all up in the background with a progress bar and a Stop button, and "Save lyrics into files" to write the lyrics Skratch holds into the audio files' own tags (synced ones as LRC text) so other players show them; a tick box saves them automatically for songs you play through and lookups you run, and stays off because it changes your files. A small line under the lyrics says where they came from (your own file, the `.lrc`, the tags, LRCLIB, NetEase or Genius), and "Try another source" asks the other service when what you got looks wrong. The player bar picks up a soft backdrop from the cover.
- **Controls:** `Space` toggles play/pause and the left/right arrow keys scrub by 5 seconds (unless you're typing). Scroll over the volume slider to change it. A single click plays a track; there's a setting to switch to double-click.

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
| `src/app/shell.rs` | Wraps the interface in a lock and runs a thread that does the playback work (track changes, media keys) while no frames are drawn, as in a minimised window |
| `src/style.rs` | Theme, title bar, record, tonearm and seek bar drawing |
| `src/artwork.rs` | Background cover loading, texture cache, accent colour |
| `src/library.rs` | SQLite index, tags, artwork cache, file edits |
| `src/tagparse.rs` | Filename pattern parser for batch tagging |
| `src/online.rs` | Cover search (iTunes, MusicBrainz, Cover Art Archive) |
| `src/health.rs` | Finds duplicate songs and unfinished tags |
| `src/genius.rs` | Plain lyrics read from genius.com song pages |
| `src/player.rs` | Headless mpv child process and JSON IPC |
| `src/download.rs` | yt-dlp jobs, progress, cancellation |
| `src/tools.rs` | Finds and installs mpv, yt-dlp and ffmpeg (setup page) |
| `src/service.rs` | Background scans and folder watching |
| `src/media.rs`, `src/queue.rs` | MPRIS, playback order |

[RoadMap.html](RoadMap.html) lists what's done and what's planned; it works offline.

## License

GPL-3.0-or-later. See [LICENSE](LICENSE). AIMP was an inspiration; no AIMP code, skins or artwork are included.

Made by [iSkuffed](https://github.com/iSkuffed).
