use notify::{EventKind, RecursiveMode, Watcher};
use skratch::{
    library::Library,
    model::{MetadataEdit, Track},
};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    time::{Duration, Instant},
};

pub enum Command {
    Refresh,
    Rescan,
    Add(PathBuf),
    Remove(PathBuf),
    Edit(i64, MetadataEdit),
    /// Many edits followed by a single rescan, for batch tagging.
    EditMany(Vec<(i64, MetadataEdit)>),
    /// Cover image for these tracks; the flag also embeds it into the audio files.
    SetCover(Vec<i64>, Vec<u8>, bool),
}

pub enum Event {
    Snapshot(Vec<PathBuf>, Vec<Track>),
    Busy(bool),
    Message(String),
    Edited(i64),
    EditFailed(i64, String),
}

pub struct Service {
    pub commands: Sender<Command>,
    pub events: Receiver<Event>,
}

impl Service {
    pub fn start(data_dir: PathBuf, wake: Arc<dyn Fn() + Send + Sync>) -> Self {
        let (commands, rx) = mpsc::channel();
        let (tx, events) = mpsc::channel();
        std::thread::spawn(move || {
            let send = |event| {
                let _ = tx.send(event);
                wake();
            };
            let mut library = match Library::open(&data_dir) {
                Ok(l) => l,
                Err(e) => {
                    send(Event::Message(format!("Cannot open library: {e:#}")));
                    return;
                }
            };
            let dirty = Arc::new(AtomicBool::new(false));
            let changed = dirty.clone();
            let ignored_dir = data_dir.clone();
            let (watch_tx, watch_rx) = mpsc::channel();
            let mut watcher =
                notify::recommended_watcher(move |result: notify::Result<notify::Event>| {
                    match result {
                        Ok(event)
                            if !matches!(event.kind, EventKind::Access(_))
                                && event
                                    .paths
                                    .iter()
                                    .any(|path| !path.starts_with(&ignored_dir)) =>
                        {
                            changed.store(true, Ordering::Relaxed);
                        }
                        Err(e) => {
                            let _ = watch_tx.send(e.to_string());
                        }
                        _ => {}
                    }
                })
                .map_err(|e| {
                    send(Event::Message(format!(
                        "Folder watching unavailable: {e}. Use Refresh."
                    )))
                })
                .ok();
            let mut watched: Vec<PathBuf> = Vec::new();
            let mut rescan = true;
            let mut force = false;
            let mut changed_at = None;
            let mut last_periodic = Instant::now();
            // Show the cached library immediately while the startup scan runs.
            if let (Ok(sources), Ok(tracks)) = (library.sources(), library.tracks()) {
                send(Event::Snapshot(sources, tracks));
            }
            loop {
                if rescan {
                    send(Event::Busy(true));
                    match library.scan(force) {
                        Ok(report) => {
                            if !report.warnings.is_empty() {
                                send(Event::Message(report.warnings.join("\n")));
                            }
                        }
                        Err(e) => send(Event::Message(format!("Scan failed: {e:#}"))),
                    }
                    match (library.sources(), library.tracks()) {
                        (Ok(sources), Ok(tracks)) => {
                            if let Some(watcher) = watcher.as_mut() {
                                for old in &watched {
                                    let _ = watcher.unwatch(old);
                                }
                                watched.clear();
                                for source in &sources {
                                    match watcher.watch(source, RecursiveMode::Recursive) {
                                        Ok(()) => watched.push(source.clone()),
                                        Err(e) => send(Event::Message(format!(
                                            "Cannot watch {}: {e}. Refresh is available.",
                                            source.display()
                                        ))),
                                    }
                                }
                            }
                            send(Event::Snapshot(sources, tracks));
                        }
                        (Err(e), _) | (_, Err(e)) => {
                            send(Event::Message(format!("Cannot read library: {e:#}")))
                        }
                    }
                    send(Event::Busy(false));
                    rescan = false;
                    force = false;
                }
                while let Ok(e) = watch_rx.try_recv() {
                    send(Event::Message(format!("Folder watcher: {e}")));
                }
                match rx.recv_timeout(Duration::from_millis(250)) {
                    Ok(command) => {
                        let result = match command {
                            Command::Refresh => {
                                force = true;
                                Ok(())
                            }
                            Command::Rescan => Ok(()),
                            Command::Add(path) => library.add_source(&path),
                            Command::Remove(path) => library.remove_source(&path),
                            Command::Edit(id, edit) => {
                                match library.edit(id, &edit) {
                                    Ok(_) => send(Event::Edited(id)),
                                    Err(e) => send(Event::EditFailed(id, format!("{e:#}"))),
                                }
                                Ok(())
                            }
                            Command::EditMany(edits) => {
                                let total = edits.len();
                                let mut failed = Vec::new();
                                for (id, edit) in edits {
                                    if let Err(e) = library.edit(id, &edit) {
                                        failed.push(format!("{}: {e:#}", edit.filename));
                                    }
                                }
                                let mut message = format!(
                                    "Updated tags on {} of {total} tracks.",
                                    total - failed.len()
                                );
                                for line in failed.iter().take(3) {
                                    message.push_str(&format!("\nSkipped {line}"));
                                }
                                send(Event::Message(message));
                                Ok(())
                            }
                            Command::SetCover(ids, image, embed) => {
                                let total = ids.len();
                                let mut failed = Vec::new();
                                for id in ids {
                                    if let Err(e) = library.set_cover(id, &image, embed) {
                                        failed.push(format!("{e:#}"));
                                    }
                                }
                                let mut message = format!(
                                    "Cover set on {} of {total} tracks.",
                                    total - failed.len()
                                );
                                if let Some(first) = failed.first() {
                                    message.push_str(&format!("\nProblem: {first}"));
                                }
                                send(Event::Message(message));
                                Ok(())
                            }
                        };
                        if let Err(e) = result {
                            send(Event::Message(format!("{e:#}")));
                        }
                        rescan = true;
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                }
                if dirty.swap(false, Ordering::Relaxed) {
                    changed_at = Some(Instant::now());
                }
                if changed_at.is_some_and(|t| t.elapsed() >= Duration::from_millis(700)) {
                    changed_at = None;
                    rescan = true;
                }
                // Reconnect offline/removable sources and recover missed watch events.
                if last_periodic.elapsed() >= Duration::from_secs(30) {
                    rescan = true;
                    last_periodic = Instant::now();
                }
            }
        });
        Self { commands, events }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, io::Write};

    fn wav(path: &std::path::Path) {
        let mut file = fs::File::create(path).unwrap();
        let pcm = [0_u8; 1600];
        file.write_all(b"RIFF").unwrap();
        file.write_all(&(36_u32 + pcm.len() as u32).to_le_bytes())
            .unwrap();
        file.write_all(b"WAVEfmt ").unwrap();
        file.write_all(&16_u32.to_le_bytes()).unwrap();
        file.write_all(&1_u16.to_le_bytes()).unwrap();
        file.write_all(&1_u16.to_le_bytes()).unwrap();
        file.write_all(&8000_u32.to_le_bytes()).unwrap();
        file.write_all(&16000_u32.to_le_bytes()).unwrap();
        file.write_all(&2_u16.to_le_bytes()).unwrap();
        file.write_all(&16_u16.to_le_bytes()).unwrap();
        file.write_all(b"data").unwrap();
        file.write_all(&(pcm.len() as u32).to_le_bytes()).unwrap();
        file.write_all(&pcm).unwrap();
    }

    fn wait_count(service: &Service, count: usize) {
        let deadline = Instant::now() + Duration::from_secs(12);
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(!remaining.is_zero(), "No snapshot with {count} tracks");
            match service
                .events
                .recv_timeout(remaining)
                .expect("library event")
            {
                Event::Snapshot(_, tracks) if tracks.len() == count => return,
                Event::Message(message) => eprintln!("library message: {message}"),
                _ => {}
            }
        }
    }

    #[test]
    fn folder_watcher_discovers_and_removes_files_without_refresh() {
        let temp = tempfile::tempdir().unwrap();
        let music = temp.path().join("music");
        fs::create_dir(&music).unwrap();
        wav(&music.join("first.wav"));
        let service = Service::start(temp.path().join("data"), Arc::new(|| {}));
        wait_count(&service, 0);
        service.commands.send(Command::Add(music.clone())).unwrap();
        wait_count(&service, 1);
        wav(&music.join("second.wav"));
        wait_count(&service, 2);
        fs::remove_file(music.join("second.wav")).unwrap();
        wait_count(&service, 1);
    }
}
