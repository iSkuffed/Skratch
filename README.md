# Skratch

A small, native, vinyl-themed music player and library for Linux and Windows. Your files stay where they are, the app is one Rust binary (no webview), and playback goes through `mpv`.

![A tour: tracks, albums, Tidy up and the lyrics page](docs/screenshots/tour.gif)

![Lyrics filling in as the song plays](docs/screenshots/lyrics-sync.gif)

## Features

- **Library:** album, artist and track views over your folders. It watches them for changes and never copies or moves your music.
- **Tags and covers:** edit tags in the file safely (temp copy, atomic rename), fill tags from filenames in bulk, and find covers on iTunes, Deezer and MusicBrainz / Cover Art Archive. Covers stay in Skratch's own cache unless you tick "Also save into the audio file".
- **Lyrics:** synced or plain, from a `.lrc` file, the file's tags, or (if you switch it on) LRCLIB, NetEase and Genius. Synced lines follow the song word by word.
- **Now playing:** a spinning record that pulses with the bass, the visualizer, lyrics, queue and library in one page.
- **Downloads:** paste a link and `yt-dlp` + `ffmpeg` save the audio. Any site yt-dlp supports; no DRM services, and a playlist link saves only its first item.
- **Tidy up:** finds duplicates and unfinished tags, suggests fixes from MusicBrainz that you apply or skip, and marks loose songs as "Singles".
- **Playback:** queue, shuffle, repeat, gapless, fades between songs, loudness levelling, sleep timer, plays and ratings, media keys.

Everything that talks to the internet is off until you ask for it or tick it. [More screenshots](docs/screenshots).

## Install

**Linux (x86_64):** download `Skratch-<version>-x86_64.AppImage` from [Releases](https://github.com/iSkuffed/Skratch/releases), run `chmod +x` on it and start it.

**Windows (64-bit, early):** download `Skratch-<version>-windows-x86_64.zip`, unzip it and run `Skratch.exe`. It isn't code-signed, so SmartScreen may warn: More info, then Run anyway. This build has had less testing than the Linux one, so please open an issue if something is off.

**From source:** needs a Rust toolchain, a C compiler, `pkg-config` and D-Bus headers on Linux (Fedora: `gcc pkgconf-pkg-config dbus-devel`, Debian/Ubuntu: `build-essential pkg-config libdbus-1-dev`).

```sh
git clone https://github.com/iSkuffed/Skratch
cd Skratch
cargo build --release
./target/release/skratch
```

The first launch has a welcome page that sets up the rest:

- **Music folder:** where your music lives.
- **mpv:** on Windows it is downloaded into Skratch's own folder. On Linux it runs your package manager (dnf, apt, pacman or zypper) through the desktop's password prompt, and shows the command if you'd rather run it yourself.
- **Downloads (optional):** fetches `yt-dlp` and `ffmpeg` from their official releases, checks their checksums and keeps them in Skratch's folder. Nothing is installed system-wide.

Your library, cached artwork and settings live in `~/.local/share/skratch` (on Windows, `%APPDATA%\skratch\data`). Set `SKRATCH_DATA_DIR` to use a separate library.

## Development

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Tests use mpv's null audio output and generated media, never your music folders. Pushing a `v*` tag runs `.github/workflows/release.yml`, which builds the AppImage and the Windows zip and attaches them to a release.

One layout rule: scroll areas use `style::scroll()`, never `egui::ScrollArea::vertical()`, so a scrollbar doesn't drift with the longest row. `clippy.toml` enforces it.

## Credits

Lookups use [MusicBrainz](https://musicbrainz.org), the [Cover Art Archive](https://coverartarchive.org), iTunes, [Deezer](https://www.deezer.com), [TheAudioDB](https://www.theaudiodb.com), [LRCLIB](https://lrclib.net), NetEase Cloud Music and [Genius](https://genius.com). The artwork and words belong to their artists and these services. Downloads use [yt-dlp](https://github.com/yt-dlp/yt-dlp) and [ffmpeg](https://ffmpeg.org).

## License

GPL-3.0-or-later. See [LICENSE](LICENSE). AIMP was an inspiration; no AIMP code, skins or artwork are included.

Made by [iSkuffed](https://github.com/iSkuffed).
