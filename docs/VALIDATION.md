# Initial validation

Validated on this Linux workspace on 2026-10-19.

## Build and automated checks

- `cargo fmt --all --check`: passed.
- `cargo clippy --all-targets -- -D warnings`: passed.
- `cargo test --all-targets`: 16 passed, none failed or ignored. Required media tools were present, so the conditional integration checks ran.
- `cargo build --release`: passed. The stripped executable is about 19 MB.

The format check generates actual WAV, FLAC, MP3, Ogg/Vorbis, Opus, M4A/AAC, raw AAC, AIFF, and MP4 containing both video and audio. All open through the headless mpv backend. An additional MP3 fixture verifies that embedded artwork takes precedence over folder art. All thumbnails are at most 256 pixels per dimension. Playback tests use null audio output.

Other checks cover shuffle traversal and repeat behavior, restoring the queue and position, folder changes appearing without a Refresh command, SQLite persistence, overlapping/offline sources, deleted files, metadata writes and readback, year removal for MP3/M4A, stable IDs after rename, filename-only renames for unsupported tag formats, collision/path validation preserving original bytes, cover updates, mpv seeking/volume/EOF/stop/cleanup, and MPRIS metadata/commands in an isolated D-Bus session.

## Native window check

The app was exercised under Xvfb using `xdotool`, `gdbus`, and screenshots. Test files were generated silence in a temporary directory. No existing music collection was accessed or modified.

Verified by interacting with the actual app:

- Add a source using the sidebar path field and index six tracks across three albums.
- Search for a title and see three matching tracks.
- Add another track directly to the watched folder and see the library reach seven tracks without Refresh.
- Restart into the same data directory and recover sources and tracks without user action or automatic audio playback.
- Start a queue, control play/pause, relative seeking, and volume through the app's MPRIS service.
- Edit a title and filename in the GUI. Confirm the new path and stable track ID in SQLite and the written title with `ffprobe`.
- Inspect the corrected album grid, seek bar, and tag editor visually.

Screenshots: [library](screenshots/library.png), [tag editor](screenshots/editor.png). The covers and music shown are synthetic fixtures, not bundled content.

The isolated D-Bus session has no functioning AT-SPI registry. AccessKit logged a background accessibility-service error; the window and media controls remained functional. This headless check does not establish screen-reader behavior on a normal desktop.

Full video display, theme/skin loading, visualizers, named playlists, and guaranteed gapless transitions are deferred. Large-library performance and listening on the user's actual audio hardware remain unmeasured.

## Resource sample

The final release was measured with seven generated tracks, three cached covers, an Xvfb X11 window and software OpenGL. Values below come from `/proc/<pid>/smaps_rollup`. PSS apportions shared mappings; RSS counts each process's resident shared mappings in full.

| State | App PSS | mpv PSS | App RSS | mpv RSS |
| --- | ---: | ---: | ---: | ---: |
| Idle after restart, no automatic playback | 53.2 MiB | no process | 85.5 MiB | no process |
| Track loaded and paused | 55.3 MiB | 55.6 MiB | 87.7 MiB | 116.6 MiB |
| After Stop | 55.5 MiB | no process | 87.7 MiB | no process |

The app plus mpv used approximately 111 MiB PSS with a track loaded. This is a small-fixture observation, not a bound for large collections or every graphics/audio driver. mpv forward/back demuxer buffers are set to 8 MiB / 1 MiB, and Stop releases mpv entirely. See [captured values](resource-check.json).

The final GUI run also verified restoring a saved position near 5 seconds, seeking forward by exactly 2 seconds while paused, stable paused position, volume 0.37 through MPRIS, double-click playback, and MPRIS Stopped with no child process after Stop. All isolated test app, mpv, D-Bus, and Xvfb processes were closed afterwards.

## Downloader and roadmap update

The integrated suite now has **26 passing checks**, with `cargo fmt --all --check` and `cargo clippy --all-targets -- -D warnings` passing. Eight downloader unit checks cover safe argument construction, URL/destination validation, one-job limits, cancellation and process-group cleanup, Drop cleanup, bounded progress, failure cleanup and no-overwrite behavior. A library check confirms staged downloads are excluded until published. The real-tool integration check runs installed yt-dlp 2026.08.19 and ffmpeg against locally served generated audio, tests original M4A and MP3 conversion, and indexes both results.

The native app was also exercised under Xvfb with an empty isolated data directory. Added a source, pasted a local HTTP audio link into Downloads, completed a download, and confirmed automatic library import. Repeating the same download preserved the original file byte-for-byte. Cancelling a deliberately delayed HTTP download removed its staging directory and left the existing song unchanged. Selecting MP3 in the actual format control produced and indexed an MP3. See [download panel](screenshots/downloads.png). No public YouTube media, cookies or signed-in session was used; public-site extractor compatibility is delegated to the installed yt-dlp.

Removed the sidebar tagline. The standalone `RoadMap.html` was opened in the T3 browser; status filtering and search worked, and the layout had no horizontal overflow at 1280px or 390px. It uses no external resources and can be opened locally without a server.

Initialized the Git-backed Jujutsu repository using `jj git init`. Build outputs remain ignored by `/target/` in `.gitignore`. No remote, bookmark, or release was created.
