//! Artist photos for the Artists page, fetched one at a time from Deezer and kept as files in
//! the app's own data (never in the music folders). Only asked when the setting is on.
use sha2::{Digest, Sha256};
use skratch::online;
use std::{
    collections::HashSet,
    fs,
    path::PathBuf,
    sync::{
        Arc,
        mpsc::{self, Receiver, Sender},
    },
    time::Duration,
};

pub struct Photos {
    dir: PathBuf,
    /// Artists whose photo file is on disk.
    have: HashSet<String>,
    /// Artists already looked at (found, missing or asked for), so each is only asked about once.
    asked: HashSet<String>,
    ask: Sender<String>,
    done: Receiver<String>,
}

impl Photos {
    pub fn new(data_dir: &std::path::Path, wake: Arc<dyn Fn() + Send + Sync>) -> Self {
        let dir = data_dir.join("artists");
        let (ask, requests) = mpsc::channel::<String>();
        let (tell, done) = mpsc::channel::<String>();
        let worker_dir = dir.clone();
        std::thread::spawn(move || {
            while let Ok(name) = requests.recv() {
                let (photo, none) = files(&worker_dir, &name);
                let _ = fs::create_dir_all(&worker_dir);
                match online::artist_photo(&name) {
                    Ok(Some(bytes)) => {
                        if fs::write(&photo, bytes).is_ok() && tell.send(name).is_err() {
                            break;
                        }
                    }
                    // nobody by that name: remembered, so it is not asked again
                    Ok(None) => {
                        let _ = fs::write(none, b"");
                    }
                    // a failure is tried again next run
                    Err(_) => {}
                }
                wake();
                std::thread::sleep(Duration::from_millis(400));
            }
        });
        Self {
            dir,
            have: HashSet::new(),
            asked: HashSet::new(),
            ask,
            done,
        }
    }

    /// The photo file of `artist` once there is one; the first call for an artist starts fetching it.
    pub fn path(&mut self, artist: &str) -> Option<PathBuf> {
        while let Ok(name) = self.done.try_recv() {
            self.have.insert(name);
        }
        if artist.trim().is_empty() || artist.starts_with("Unknown") {
            return None;
        }
        if !self.asked.contains(artist) {
            self.asked.insert(artist.to_owned());
            let (photo, none) = files(&self.dir, artist);
            if photo.exists() {
                self.have.insert(artist.to_owned());
            } else if !none.exists() {
                let _ = self.ask.send(artist.to_owned());
            }
        }
        self.have
            .contains(artist)
            .then(|| files(&self.dir, artist).0)
    }
}

/// Where an artist's photo is kept, and the marker that says there is none.
fn files(dir: &std::path::Path, artist: &str) -> (PathBuf, PathBuf) {
    let hash = Sha256::digest(artist.to_lowercase().as_bytes());
    let name: String = hash.iter().take(10).map(|b| format!("{b:02x}")).collect();
    (
        dir.join(format!("{name}.jpg")),
        dir.join(format!("{name}.none")),
    )
}
