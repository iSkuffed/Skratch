use crate::{
    artwork::Artwork,
    service::{self, Command, Event},
    state::SavedState,
    style::{self, Icon},
};
use eframe::egui::{self, Color32, CornerRadius, Frame, Margin, RichText};
use skratch::{
    download::{DownloadEvent, DownloadFormat, DownloadRequest, Downloader},
    media::{DesktopControls, MediaCommand},
    model::{MetadataEdit, Track},
    online::{self, Candidate},
    player::{Player, PlayerEvent},
    queue::Repeat,
    tagparse::{self, Parsed},
};
use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    path::PathBuf,
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

const SEARCH_ID: &str = "library-search";

#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    Albums,
    Artists,
    Tracks,
    Queue,
    Downloads,
}
impl View {
    fn title(self) -> &'static str {
        match self {
            Self::Albums => "Albums",
            Self::Artists => "Artists",
            Self::Tracks => "All tracks",
            Self::Queue => "Play queue",
            Self::Downloads => "Downloads",
        }
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Sort {
    Album,
    Artist,
    Title,
    Year,
    Filename,
}
impl Sort {
    fn title(self) -> &'static str {
        match self {
            Self::Album => "Album / track",
            Self::Artist => "Artist",
            Self::Title => "Title",
            Self::Year => "Year",
            Self::Filename => "Filename",
        }
    }
}
/// One selected track in the filename-pattern preview: id, filename stem, and the parsed result.
type FillRow = (i64, String, Option<(Parsed, MetadataEdit)>);

struct Group {
    name: String,
    subtitle: String,
    indices: Vec<usize>,
}
struct Editor {
    id: i64,
    edit: MetadataEdit,
    year: String,
    track: String,
    disc: String,
    error: String,
    saving: bool,
}

/// Messages from the cover-search worker threads.
enum CoverMsg {
    Found(Result<Vec<Candidate>, String>),
    Thumb(usize, Option<egui::ColorImage>),
    Full(Result<Vec<u8>, String>),
}
struct Found {
    candidate: Candidate,
    texture: Option<egui::TextureHandle>,
    failed: bool,
}
/// One album (or single) to find a cover for.
struct CoverItem {
    ids: Vec<i64>,
    artist: String,
    query: String,
}
struct CoverSearch {
    /// Albums still waiting after the current one, and how many there were in total.
    rest: VecDeque<CoverItem>,
    total: usize,
    ids: Vec<i64>,
    artist: String,
    query: String,
    found: Vec<Found>,
    chosen: Option<usize>,
    status: String,
    working: bool,
    rx: Option<mpsc::Receiver<CoverMsg>>,
}
impl CoverSearch {
    /// Switches to `item` and searches for it right away.
    fn begin(&mut self, item: CoverItem, wake: Arc<dyn Fn() + Send + Sync>) {
        self.rx = Some(spawn_cover_search(
            item.artist.clone(),
            item.query.clone(),
            wake,
        ));
        self.ids = item.ids;
        self.artist = item.artist;
        self.query = item.query;
        self.found.clear();
        self.chosen = None;
        self.working = true;
        self.status = "Searching...".into();
    }
}

enum Action {
    FindCover(Vec<i64>),
    StartVisible(usize),
    Enqueue(i64),
    Select(usize),
    Edit(i64),
    Pause,
    Next,
    Previous,
    Stop,
    Seek(f64),
    Volume(f64),
}

pub struct Skratch {
    service: service::Service,
    tracks: Vec<Track>,
    by_id: HashMap<i64, usize>,
    sources: Vec<PathBuf>,
    state: SavedState,
    state_path: PathBuf,
    player: Option<Player>,
    desktop: Option<DesktopControls>,
    wake: Arc<dyn Fn() + Send + Sync>,
    playing: bool,
    loaded: bool,
    pending_seek: Option<f64>,
    duration: f64,
    view: View,
    sort: Sort,
    descending: bool,
    search: String,
    filtered: Vec<usize>,
    groups: Vec<Group>,
    selected_group: Option<usize>,
    art: Artwork,
    busy: bool,
    messages: Vec<String>,
    editor: Option<Editor>,
    source_text: String,
    folder_rx: Option<mpsc::Receiver<Option<PathBuf>>>,
    action: Option<Action>,
    last_save: Instant,
    last_desktop: Instant,
    downloader: Downloader,
    download_url: String,
    download_status: String,
    download_log: std::collections::VecDeque<String>,
    select_mode: bool,
    selected: HashSet<i64>,
    fill_open: bool,
    cover: Option<CoverSearch>,
    logo: eframe::egui::TextureHandle,
    accent: Color32,
    accent_applied: Color32,
    spin: f32,
    spin_speed: f32,
    arm: f32,
}

impl Skratch {
    pub fn new(cc: &eframe::CreationContext<'_>, data_dir: PathBuf) -> Self {
        let ctx = cc.egui_ctx.clone();
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(move || ctx.request_repaint());
        style::apply(&cc.egui_ctx, style::ACCENT);
        let state_path = data_dir.join("player.json");
        let mut messages = Vec::new();
        let state = match SavedState::load(&state_path) {
            Ok(state) => state,
            Err(e) => {
                let backup = state_path.with_extension(format!(
                    "invalid-{}",
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs()
                ));
                match std::fs::rename(&state_path, &backup) {
                    Ok(()) => messages.push(format!(
                        "{e:#}. Saved a recovery copy at {}.",
                        backup.display()
                    )),
                    Err(copy_error) => messages.push(format!(
                        "{e:#}. Could not preserve invalid state: {copy_error}"
                    )),
                }
                SavedState {
                    volume: 70.0,
                    ..Default::default()
                }
            }
        };
        let mut state = state;
        if state.tag_pattern.trim().is_empty() {
            state.tag_pattern = tagparse::DEFAULT_PATTERN.into();
        }
        let desktop = match DesktopControls::new(wake.clone()) {
            Ok(controls) => Some(controls),
            Err(e) => {
                messages.push(format!("Desktop media controls unavailable: {e:#}"));
                None
            }
        };
        Self {
            service: service::Service::start(data_dir, wake.clone()),
            tracks: Vec::new(),
            by_id: HashMap::new(),
            sources: Vec::new(),
            state,
            state_path,
            player: None,
            desktop,
            downloader: Downloader::new(wake.clone()),
            download_url: String::new(),
            download_status: String::new(),
            download_log: std::collections::VecDeque::new(),
            wake,
            playing: false,
            loaded: false,
            pending_seek: None,
            duration: 0.0,
            view: View::Albums,
            sort: Sort::Album,
            descending: false,
            search: String::new(),
            filtered: Vec::new(),
            groups: Vec::new(),
            selected_group: None,
            art: Artwork::new(cc.egui_ctx.clone()),
            busy: false,
            messages,
            editor: None,
            source_text: String::new(),
            folder_rx: None,
            action: None,
            last_save: Instant::now(),
            last_desktop: Instant::now(),
            select_mode: false,
            selected: HashSet::new(),
            fill_open: false,
            cover: None,
            logo: style::load_logo(&cc.egui_ctx),
            accent: style::ACCENT,
            accent_applied: style::ACCENT,
            spin: 0.0,
            spin_speed: 0.0,
            arm: 0.0,
        }
    }

    fn current(&self) -> Option<&Track> {
        self.state
            .queue
            .current_id()
            .and_then(|id| self.by_id.get(&id))
            .map(|i| &self.tracks[*i])
    }

    fn message(&mut self, text: String) {
        if self.messages.last() != Some(&text) {
            self.messages.push(text);
        }
        if self.messages.len() > 12 {
            self.messages.remove(0);
        }
    }

    fn command(&mut self, command: Command) {
        if self.service.commands.send(command).is_err() {
            self.message("The library worker has stopped. Restart Skratch to retry.".into());
        }
    }

    fn rebuild(&mut self) {
        let selected = self
            .selected_group
            .and_then(|i| self.groups.get(i))
            .map(|g| (g.name.clone(), g.subtitle.clone()));
        let query = self.search.to_lowercase();
        self.filtered = self
            .tracks
            .iter()
            .enumerate()
            .filter(|(_, t)| {
                query.is_empty()
                    || [&t.title, &t.artist, &t.album, &t.album_artist, &t.genre]
                        .iter()
                        .any(|s| s.to_lowercase().contains(&query))
                    || t.path.to_string_lossy().to_lowercase().contains(&query)
            })
            .map(|(i, _)| i)
            .collect();
        self.filtered.sort_by(|a, b| {
            let a = &self.tracks[*a];
            let b = &self.tracks[*b];
            let cmp = match self.sort {
                Sort::Album => a
                    .album
                    .to_lowercase()
                    .cmp(&b.album.to_lowercase())
                    .then_with(|| {
                        a.album_artist
                            .to_lowercase()
                            .cmp(&b.album_artist.to_lowercase())
                    }),
                Sort::Artist => a
                    .artist
                    .to_lowercase()
                    .cmp(&b.artist.to_lowercase())
                    .then_with(|| a.album.to_lowercase().cmp(&b.album.to_lowercase())),
                Sort::Title => a.title.to_lowercase().cmp(&b.title.to_lowercase()),
                Sort::Year => a
                    .year
                    .cmp(&b.year)
                    .then_with(|| a.album.to_lowercase().cmp(&b.album.to_lowercase())),
                Sort::Filename => a.path.cmp(&b.path),
            }
            .then_with(|| a.disc_number.cmp(&b.disc_number))
            .then_with(|| a.track_number.cmp(&b.track_number))
            .then_with(|| a.path.cmp(&b.path));
            if self.descending { cmp.reverse() } else { cmp }
        });
        let mut groups: BTreeMap<(String, String), Vec<usize>> = BTreeMap::new();
        for i in &self.filtered {
            let t = &self.tracks[*i];
            let key = if self.view == View::Artists {
                (
                    display(&t.artist, "Unknown artist").to_owned(),
                    String::new(),
                )
            } else {
                (
                    display(&t.album, "Unknown album").to_owned(),
                    display(
                        if t.album_artist.is_empty() {
                            &t.artist
                        } else {
                            &t.album_artist
                        },
                        "Unknown artist",
                    )
                    .to_owned(),
                )
            };
            groups.entry(key).or_default().push(*i);
        }
        self.groups = groups
            .into_iter()
            .map(|((name, subtitle), indices)| Group {
                name,
                subtitle,
                indices,
            })
            .collect();
        // Group order follows the selected track sort, including year and descending order.
        let ranks: HashMap<usize, usize> = self
            .filtered
            .iter()
            .enumerate()
            .map(|(rank, index)| (*index, rank))
            .collect();
        self.groups.sort_by_key(|g| {
            g.indices
                .first()
                .and_then(|i| ranks.get(i))
                .copied()
                .unwrap_or(usize::MAX)
        });
        self.selected_group = selected.and_then(|key| {
            self.groups.iter().position(|g| {
                (g.name.as_str(), g.subtitle.as_str()) == (key.0.as_str(), key.1.as_str())
            })
        });
    }

    fn start_current(&mut self, position: f64) {
        let Some(track) = self.current().cloned() else {
            return;
        };
        if self.player.is_none() {
            match Player::start(self.wake.clone()) {
                Ok(player) => self.player = Some(player),
                Err(e) => {
                    self.message(format!("Cannot start playback: {e:#}"));
                    return;
                }
            }
        }
        let player = self.player.as_mut().unwrap();
        let result = player
            .set_volume(self.state.volume)
            .and_then(|_| player.load(&track.path));
        match result {
            Ok(()) => {
                self.loaded = true;
                self.playing = true;
                self.duration = track.duration;
                self.state.position = position;
                self.pending_seek = (position > 0.0).then_some(position);
            }
            Err(e) => {
                let _ = player.stop();
                self.playing = false;
                self.loaded = false;
                self.message(format!("Cannot play {}: {e:#}", track.path.display()));
            }
        }
        self.last_desktop = Instant::now() - Duration::from_secs(2);
    }

    fn perform(&mut self, action: Action) {
        let result = match action {
            Action::StartVisible(index) => {
                let indices = self
                    .selected_group
                    .and_then(|i| self.groups.get(i))
                    .map(|g| &g.indices)
                    .unwrap_or(&self.filtered);
                let ids = indices.iter().map(|i| self.tracks[*i].id).collect();
                self.state.queue.start(ids, index);
                self.start_current(0.0);
                Ok(())
            }
            Action::FindCover(ids) => {
                self.open_cover(ids);
                Ok(())
            }
            Action::Enqueue(id) => {
                self.state.queue.enqueue(id);
                Ok(())
            }
            Action::Select(index) => {
                self.state.queue.select(index);
                self.start_current(0.0);
                Ok(())
            }
            Action::Pause => {
                if !self.loaded {
                    self.start_current(self.state.position);
                    Ok(())
                } else if let Some(player) = self.player.as_mut() {
                    let result = player.set_paused(self.playing);
                    if result.is_ok() {
                        self.playing = !self.playing;
                    }
                    result
                } else {
                    Ok(())
                }
            }
            Action::Next => {
                if self.state.queue.advance(false).is_some() {
                    self.start_current(0.0);
                } else {
                    self.stop();
                }
                Ok(())
            }
            Action::Previous => {
                if self.state.position > 3.0 {
                    return self.perform(Action::Seek(0.0));
                }
                if self.state.queue.previous().is_some() {
                    self.start_current(0.0);
                }
                Ok(())
            }
            Action::Stop => {
                self.stop();
                Ok(())
            }
            Action::Seek(position) => {
                self.state.position = if self.duration > 0.0 {
                    position.max(0.0).min(self.duration)
                } else {
                    position.max(0.0)
                };
                if self.loaded {
                    self.player.as_mut().unwrap().seek(self.state.position)
                } else {
                    Ok(())
                }
            }
            Action::Volume(volume) => {
                self.state.volume = volume.clamp(0.0, 100.0);
                if let Some(player) = self.player.as_mut() {
                    player.set_volume(self.state.volume)
                } else {
                    Ok(())
                }
            }
            Action::Edit(id) => {
                if let Some(i) = self.by_id.get(&id) {
                    let edit = MetadataEdit::from(&self.tracks[*i]);
                    self.editor = Some(Editor {
                        id,
                        year: edit.year.map(|n| n.to_string()).unwrap_or_default(),
                        track: edit.track_number.map(|n| n.to_string()).unwrap_or_default(),
                        disc: edit.disc_number.map(|n| n.to_string()).unwrap_or_default(),
                        edit,
                        error: String::new(),
                        saving: false,
                    });
                }
                Ok(())
            }
        };
        if let Err(e) = result {
            self.message(format!("Playback command failed: {e:#}"));
        }
        self.last_desktop = Instant::now() - Duration::from_secs(2);
    }

    fn stop(&mut self) {
        if let Some(mut player) = self.player.take()
            && let Err(e) = player.stop()
        {
            self.message(format!("Cannot stop playback: {e:#}"));
        }
        self.playing = false;
        self.loaded = false;
        self.state.position = 0.0;
        self.pending_seek = None;
    }

    fn poll(&mut self, ctx: &egui::Context) {
        while let Ok(event) = self.service.events.try_recv() {
            match event {
                Event::Busy(value) => self.busy = value,
                Event::Message(message) => self.message(message),
                Event::Edited(id) => {
                    if self.editor.as_ref().is_some_and(|editor| editor.id == id) {
                        self.editor = None;
                    }
                }
                Event::EditFailed(id, error) => {
                    if let Some(editor) = self.editor.as_mut().filter(|editor| editor.id == id) {
                        editor.error = error;
                        editor.saving = false;
                    } else {
                        self.message(error);
                    }
                }
                Event::Snapshot(sources, tracks) => {
                    self.sources = sources;
                    self.tracks = tracks;
                    self.art.invalidate();
                    self.by_id = self
                        .tracks
                        .iter()
                        .enumerate()
                        .map(|(i, t)| (t.id, i))
                        .collect();
                    self.state.queue.retain(|id| self.by_id.contains_key(&id));
                    if self.loaded && self.state.queue.current_id().is_none() {
                        self.stop();
                    }
                    self.rebuild();
                }
            }
        }
        while let Ok(event) = self.downloader.events.try_recv() {
            match event {
                DownloadEvent::Log(line) => {
                    self.download_status = line.clone();
                    self.download_log.push_back(line);
                    while self.download_log.len() > 60 {
                        self.download_log.pop_front();
                    }
                }
                DownloadEvent::Finished(path) => {
                    self.download_status = format!("Saved {}", path.display());
                    self.command(Command::Rescan);
                }
                DownloadEvent::Failed(error) => {
                    self.download_status = format!("Download failed: {error}")
                }
                DownloadEvent::Cancelled => self.download_status = "Download cancelled.".into(),
            }
        }
        if let Some(rx) = self.folder_rx.as_ref() {
            match rx.try_recv() {
                Ok(path) => {
                    self.folder_rx = None;
                    if let Some(path) = path {
                        self.command(Command::Add(path));
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => self.folder_rx = None,
                _ => {}
            }
        }
        loop {
            let event = self.player.as_ref().and_then(|p| p.events.try_recv().ok());
            let Some(event) = event else {
                break;
            };
            match event {
                PlayerEvent::Started => {
                    if let Some(position) = self.pending_seek.take() {
                        self.perform(Action::Seek(position));
                    }
                }
                PlayerEvent::Position(position) => {
                    if self.loaded && self.pending_seek.is_none() && position.is_finite() {
                        self.state.position = position.max(0.0);
                    }
                }
                PlayerEvent::Duration(duration) => {
                    if duration.is_finite() {
                        self.duration = duration.max(0.0);
                    }
                }
                PlayerEvent::Paused(paused) => {
                    if self.loaded {
                        self.playing = !paused;
                    }
                }
                PlayerEvent::Volume(volume) => self.state.volume = volume.clamp(0.0, 100.0),
                PlayerEvent::Ended if self.loaded => {
                    if self.state.queue.advance(true).is_some() {
                        self.start_current(0.0);
                    } else {
                        self.stop();
                    }
                }
                PlayerEvent::Ended => {}
                PlayerEvent::Error(error) => {
                    self.message(error);
                    self.playing = false;
                    self.loaded = false;
                    self.player = None;
                }
            }
        }
        loop {
            let event = self.desktop.as_ref().and_then(|d| d.events.try_recv().ok());
            let Some(event) = event else {
                break;
            };
            match event {
                MediaCommand::Play if !self.playing => self.perform(Action::Pause),
                MediaCommand::Pause if self.playing => self.perform(Action::Pause),
                MediaCommand::Toggle => self.perform(Action::Pause),
                MediaCommand::Next => self.perform(Action::Next),
                MediaCommand::Previous => self.perform(Action::Previous),
                MediaCommand::Stop => self.perform(Action::Stop),
                MediaCommand::SeekBy(seconds) => {
                    self.perform(Action::Seek(self.state.position + seconds))
                }
                MediaCommand::SeekTo(seconds) => self.perform(Action::Seek(seconds)),
                MediaCommand::SetVolume(volume) => self.perform(Action::Volume(volume)),
                MediaCommand::Raise => ctx.send_viewport_cmd(egui::ViewportCommand::Focus),
                MediaCommand::Quit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                _ => {}
            }
        }
        if self.last_desktop.elapsed() >= Duration::from_secs(1) {
            let track = if self.loaded {
                self.current().cloned()
            } else {
                None
            };
            if let Some(desktop) = self.desktop.as_mut()
                && let Err(e) = desktop.update(
                    track.as_ref(),
                    self.playing,
                    self.state.position,
                    self.state.volume,
                )
            {
                self.message(format!("Desktop controls: {e:#}"));
            }
            self.last_desktop = Instant::now();
        }
        if self.last_save.elapsed() >= Duration::from_secs(5) {
            self.save();
            self.last_save = Instant::now();
        }
        self.art.drain(ctx);
    }

    fn save(&mut self) {
        if let Err(e) = self.state.save(&self.state_path) {
            self.message(format!("Cannot save playback state: {e:#}"));
        }
    }

    fn sidebar(&mut self, ui: &mut egui::Ui) {
        let frame = Frame::side_top_panel(ui.style())
            .fill(style::SIDEBAR)
            .inner_margin(Margin::same(14));
        egui::SidePanel::left("library-sidebar")
            .default_width(215.0)
            .width_range(190.0..=300.0)
            .frame(frame)
            .show_inside(ui, |ui| {
                ui.add_space(8.0);
                section(ui, "BROWSE", false);
                for view in [
                    View::Albums,
                    View::Artists,
                    View::Tracks,
                    View::Queue,
                    View::Downloads,
                ] {
                    if ui
                        .selectable_label(self.view == view, RichText::new(view.title()).size(16.0))
                        .clicked()
                    {
                        self.view = view;
                        self.selected_group = None;
                        self.selected.clear();
                        self.rebuild();
                    }
                }
                section(ui, "MUSIC FOLDERS", true);
                let mut remove = None;
                egui::ScrollArea::vertical()
                    .id_salt("sources")
                    .max_height(180.0)
                    .show(ui, |ui| {
                        for path in &self.sources {
                            ui.horizontal(|ui| {
                                ui.label(
                                    path.file_name()
                                        .unwrap_or(path.as_os_str())
                                        .to_string_lossy(),
                                )
                                .on_hover_text(path.display().to_string());
                                if ui
                                    .small_button("x")
                                    .on_hover_text("Remove source from library. Files are kept.")
                                    .clicked()
                                {
                                    remove = Some(path.clone());
                                }
                            });
                        }
                    });
                if let Some(path) = remove {
                    self.command(Command::Remove(path));
                }
                if ui
                    .add_enabled(
                        self.folder_rx.is_none(),
                        egui::Button::new("+ Choose folder"),
                    )
                    .clicked()
                {
                    let (tx, rx) = mpsc::channel();
                    self.folder_rx = Some(rx);
                    let wake = self.wake.clone();
                    std::thread::spawn(move || {
                        let folder = rfd::FileDialog::new()
                            .set_title("Add a music folder")
                            .pick_folder();
                        let _ = tx.send(folder);
                        wake();
                    });
                }
                ui.add(
                    egui::TextEdit::singleline(&mut self.source_text)
                        .hint_text("Or paste an absolute path")
                        .desired_width(f32::INFINITY),
                );
                if ui
                    .add_enabled(
                        !self.source_text.trim().is_empty(),
                        egui::Button::new("Add path"),
                    )
                    .clicked()
                {
                    let path = PathBuf::from(self.source_text.trim());
                    self.command(Command::Add(path));
                    self.source_text.clear();
                }
                section(ui, "LIBRARY", true);
                ui.label(
                    RichText::new(format!("{} tracks", self.tracks.len())).color(style::CREAM),
                );
                if self.busy {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Updating library...");
                    });
                }
                if ui
                    .add_enabled(!self.busy, egui::Button::new("Refresh library"))
                    .clicked()
                {
                    self.command(Command::Refresh);
                }
                ui.label(
                    RichText::new("Folders update automatically.")
                        .small()
                        .weak(),
                );
                section(ui, "SETTINGS", true);
                ui.checkbox(&mut self.state.double_click, "Double-click to play")
                    .on_hover_text("Off: a single click starts a track.");
            });
    }

    /// Eases the record, tonearm and accent colour towards their targets for this frame.
    fn animate(&mut self, ctx: &egui::Context) {
        let dt = ctx.input(|i| i.stable_dt).min(0.1);
        let ease = |value: &mut f32, target: f32, rate: f32| {
            *value += (target - *value) * (1.0 - (-rate * dt).exp());
        };
        let playing = if self.playing { 1.0 } else { 0.0 };
        ease(&mut self.spin_speed, playing, 2.5);
        ease(&mut self.arm, playing, 3.0);
        // 33 1/3 rpm
        self.spin = (self.spin + self.spin_speed * 3.49 * dt) % std::f32::consts::TAU;
        let target = match self.current() {
            Some(t) => t
                .artwork
                .as_deref()
                .and_then(|path| self.art.accent(path))
                .unwrap_or_else(|| style::seed_color(&format!("{}{}", t.artist, t.album))),
            None => style::ACCENT,
        };
        self.accent = self.accent.lerp_to_gamma(target, 1.0 - (-6.0 * dt).exp());
        let close = |a: Color32, b: Color32| {
            a.r()
                .abs_diff(b.r())
                .max(a.g().abs_diff(b.g()))
                .max(a.b().abs_diff(b.b()))
                < 2
        };
        if close(self.accent, target) {
            self.accent = target;
        }
        if self.accent != self.accent_applied {
            style::apply(ctx, self.accent);
            self.accent_applied = self.accent;
            ctx.request_repaint();
        }
        if self.spin_speed > 0.005 || (self.arm - playing).abs() > 0.005 {
            ctx.request_repaint();
        }
    }

    fn title_bar(&mut self, ui: &mut egui::Ui, radius: u8) {
        let now = self
            .current()
            .map(|t| format!("{}  -  {}", t.title, display(&t.artist, "Unknown artist")))
            .unwrap_or_default();
        let frame = Frame::new().fill(style::TITLE).corner_radius(CornerRadius {
            nw: radius,
            ne: radius,
            sw: 0,
            se: 0,
        });
        egui::TopBottomPanel::top("titlebar")
            .exact_height(44.0)
            .frame(frame)
            .show_inside(ui, |ui| style::title_bar(ui, &self.logo, self.accent, &now));
    }

    fn player_bar(&mut self, ui: &mut egui::Ui, radius: u8) {
        let track = self.current().cloned();
        let accent = self.accent;
        let art = track
            .as_ref()
            .and_then(|t| t.artwork.as_deref())
            .and_then(|path| self.art.texture(path));
        let label = track.as_ref().map_or(accent, |t| {
            style::seed_color(&format!("{}{}", t.artist, t.album))
        });
        let frame = Frame::new()
            .fill(style::BAR)
            .inner_margin(Margin::symmetric(18, 10))
            .corner_radius(CornerRadius {
                nw: 0,
                ne: 0,
                sw: radius,
                se: radius,
            });
        egui::TopBottomPanel::bottom("player")
            .exact_height(136.0)
            .frame(frame)
            .show_inside(ui, |ui| {
                ui.horizontal(|ui| {
                    let (rect, _) =
                        ui.allocate_exact_size(egui::vec2(160.0, 112.0), egui::Sense::hover());
                    let center = egui::pos2(rect.left() + 56.0, rect.center().y);
                    style::record(ui.painter(), center, 52.0, art, label, self.spin);
                    style::tonearm(
                        ui.painter(),
                        egui::pos2(rect.left() + 148.0, rect.top() + 2.0),
                        self.arm,
                        accent,
                    );
                    ui.add_space(6.0);
                    ui.vertical(|ui| {
                        ui.horizontal(|ui| {
                            ui.vertical(|ui| {
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(
                                            track
                                                .as_ref()
                                                .map(|t| t.title.as_str())
                                                .unwrap_or("Choose something to play"),
                                        )
                                        .size(19.0)
                                        .strong()
                                        .color(style::CREAM),
                                    )
                                    .truncate(),
                                );
                                ui.label(
                                    RichText::new(
                                        track
                                            .as_ref()
                                            .map(|t| display(&t.artist, "Unknown artist"))
                                            .unwrap_or("Skratch"),
                                    )
                                    .size(14.0)
                                    .color(accent),
                                );
                            });
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.spacing_mut().slider_width = 110.0;
                                    let mut volume = self.state.volume;
                                    let slider = ui.add(
                                        egui::Slider::new(&mut volume, 0.0..=100.0).text("Volume"),
                                    );
                                    // scroll wheel: about 2 points per notch
                                    let wheel = if slider.hovered() {
                                        ui.input(|i| i.raw_scroll_delta.y)
                                    } else {
                                        0.0
                                    };
                                    if wheel != 0.0 {
                                        volume =
                                            (volume + f64::from(wheel) / 40.0).clamp(0.0, 100.0);
                                    }
                                    if slider.changed() || wheel != 0.0 {
                                        self.action = Some(Action::Volume(volume));
                                    }
                                    let label = match self.state.queue.repeat {
                                        Repeat::Off => "Repeat: off",
                                        Repeat::All => "Repeat: all",
                                        Repeat::One => "Repeat: one",
                                    };
                                    if ui.button(label).clicked() {
                                        self.state.queue.repeat = match self.state.queue.repeat {
                                            Repeat::Off => Repeat::All,
                                            Repeat::All => Repeat::One,
                                            Repeat::One => Repeat::Off,
                                        };
                                    }
                                    let mut shuffle = self.state.queue.shuffle;
                                    if ui.toggle_value(&mut shuffle, "Shuffle").clicked() {
                                        self.state.queue.set_shuffle(shuffle);
                                    }
                                },
                            );
                        });
                        ui.add_space(4.0);
                        let layout = egui::Layout::left_to_right(egui::Align::Center);
                        ui.allocate_ui_with_layout(
                            egui::vec2(ui.available_width(), 46.0),
                            layout,
                            |ui| {
                                ui.spacing_mut().item_spacing.x = 4.0;
                                if style::icon_button(ui, Icon::Prev, 34.0, None, true).clicked() {
                                    self.action = Some(Action::Previous);
                                }
                                let icon = if self.playing {
                                    Icon::Pause
                                } else {
                                    Icon::Play
                                };
                                if style::icon_button(ui, icon, 46.0, Some(accent), track.is_some())
                                    .clicked()
                                {
                                    self.action = Some(Action::Pause);
                                }
                                if style::icon_button(ui, Icon::Next, 34.0, None, true).clicked() {
                                    self.action = Some(Action::Next);
                                }
                                if style::icon_button(ui, Icon::Stop, 34.0, None, true).clicked() {
                                    self.action = Some(Action::Stop);
                                }
                                ui.add_space(10.0);
                                ui.label(time(self.state.position));
                                ui.add_space(4.0);
                                // reserve room for the duration label after the grooves
                                let width = (ui.available_width() - 56.0).max(60.0);
                                let seed = track.as_ref().map_or(0, |t| t.id as u64);
                                ui.allocate_ui(egui::vec2(width, 30.0), |ui| {
                                    if let Some(position) = style::groove_seek(
                                        ui,
                                        self.state.position,
                                        self.duration,
                                        seed,
                                        accent,
                                    ) {
                                        self.action = Some(Action::Seek(position));
                                    }
                                });
                                ui.label(time(self.duration));
                            },
                        );
                    });
                });
            });
    }

    /// "Select" toggle plus the batch actions, shown above any track list. `ids` are the visible tracks.
    fn select_bar(&mut self, ui: &mut egui::Ui, ids: Vec<i64>) {
        ui.horizontal(|ui| {
            if ui.selectable_label(self.select_mode, "Select").clicked() {
                self.select_mode = !self.select_mode;
                if !self.select_mode {
                    self.selected.clear();
                }
            }
            if self.select_mode {
                if ui.button("Select all").clicked() {
                    self.selected.extend(ids.iter().copied());
                }
                if ui.button("Clear").clicked() {
                    self.selected.clear();
                }
                ui.label(format!("{} selected", self.selected.len()));
                if ui
                    .add_enabled(
                        !self.selected.is_empty(),
                        egui::Button::new("Fix tags from filenames..."),
                    )
                    .clicked()
                {
                    self.fill_open = true;
                }
                if ui
                    .add_enabled(
                        !self.selected.is_empty(),
                        egui::Button::new("Find cover online..."),
                    )
                    .clicked()
                {
                    let ids = self
                        .tracks
                        .iter()
                        .map(|t| t.id)
                        .filter(|id| self.selected.contains(id))
                        .collect();
                    self.action = Some(Action::FindCover(ids));
                }
            }
        });
    }

    /// Previews a filename pattern on the selected tracks, then writes the tags on request.
    fn fill_window(&mut self, ctx: &egui::Context) {
        if !self.fill_open {
            return;
        }
        let pattern = self.state.tag_pattern.clone();
        let rows: Vec<FillRow> = self
            .tracks
            .iter()
            .filter(|t| self.selected.contains(&t.id))
            .map(|t| {
                let stem = t
                    .path
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned();
                let result = tagparse::parse(&pattern, &stem).map(|p| {
                    let edit = p.apply(&MetadataEdit::from(t));
                    (p, edit)
                });
                (t.id, stem, result)
            })
            .collect();
        let matched = rows.iter().filter(|r| r.2.is_some()).count();
        let (mut open, mut apply) = (true, false);
        egui::Window::new("Fix tags from filenames")
            .open(&mut open)
            .default_pos(ctx.content_rect().center() - egui::vec2(340.0, 230.0))
            .collapsible(false)
            .default_width(680.0)
            .show(ctx, |ui| {
                ui.label("Filename pattern");
                ui.add(egui::TextEdit::singleline(&mut self.state.tag_pattern).desired_width(f32::INFINITY).font(egui::TextStyle::Monospace));
                ui.horizontal(|ui| {
                    for preset in ["{artist} - {title}", "{title} - {artist}", "{track}. {artist} - {title}", "{artist} - {album} - {title}"] {
                        if ui.small_button(preset).clicked() {
                            self.state.tag_pattern = preset.into();
                        }
                    }
                });
                ui.label(RichText::new("Placeholders: {artist} {title} {album} {track}. A featured artist (ft. / feat. / featuring) is found automatically and added to the title as (feat. X).").small().weak());
                ui.separator();
                egui::ScrollArea::vertical().max_height(320.0).show_rows(ui, 20.0, rows.len(), |ui, range| {
                    for (_, stem, result) in &rows[range] {
                        ui.horizontal(|ui| {
                            ui.allocate_ui_with_layout(egui::vec2(280.0, 18.0), egui::Layout::left_to_right(egui::Align::Center), |ui| { ui.set_min_size(egui::vec2(280.0, 18.0)); ui.add(egui::Label::new(RichText::new(stem).weak()).truncate()); });
                            match result {
                                Some((parsed, _)) => {
                                    let mut text = format!(
                                        "{}  -  {}",
                                        parsed.artist.as_deref().unwrap_or("(keep artist)"),
                                        parsed.title.as_deref().unwrap_or("(keep title)")
                                    );
                                    if let Some(album) = &parsed.album { text.push_str(&format!("   [{album}]")); }
                                    if let Some(track) = parsed.track_number { text.push_str(&format!("   #{track}")); }
                                    ui.add(egui::Label::new(RichText::new(text).color(self.accent)).truncate());
                                }
                                None => { ui.label(RichText::new("no match, will be left alone").color(Color32::from_rgb(190, 110, 90))); }
                            }
                        });
                    }
                });
                ui.separator();
                ui.label(format!("{matched} of {} tracks match. Tags are written into the files themselves; filenames stay the same.", rows.len()));
                ui.horizontal(|ui| {
                    if ui.add_enabled(matched > 0, egui::Button::new(format!("Apply to {matched} tracks"))).clicked() {
                        apply = true;
                    }
                    if ui.button("Cancel").clicked() {
                        self.fill_open = false;
                    }
                });
            });
        if apply {
            let edits = rows
                .into_iter()
                .filter_map(|(id, _, r)| r.map(|(_, edit)| (id, edit)))
                .collect();
            self.command(Command::EditMany(edits));
            self.fill_open = false;
            self.select_mode = false;
            self.selected.clear();
        } else if !open {
            self.fill_open = false;
        }
    }

    fn track_row(
        &mut self,
        ui: &mut egui::Ui,
        index: usize,
        queue_index: Option<usize>,
        context_index: usize,
    ) {
        const HEIGHT: f32 = 40.0;
        let track = &self.tracks[index];
        let id = track.id;
        let selected = self.state.queue.current_id() == Some(id);
        let (rect, response) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), HEIGHT),
            egui::Sense::click(),
        );
        let response = response.on_hover_text(track.path.display().to_string());
        let painter = ui.painter();
        if selected {
            painter.rect_filled(rect, 8.0, self.accent.gamma_multiply(0.22));
        } else if response.hovered() {
            painter.rect_filled(rect, 8.0, style::RAISED.gamma_multiply(0.7));
        }
        let picking = self.select_mode;
        let picked = self.selected.contains(&id);
        let lead = if picking { 30.0 } else { 0.0 };
        if picked {
            ui.painter()
                .rect_filled(rect, 8.0, self.accent.gamma_multiply(0.14));
        }
        if picking {
            let boxed = egui::Rect::from_center_size(
                rect.left_center() + egui::vec2(18.0, 0.0),
                egui::vec2(16.0, 16.0),
            );
            let painter = ui.painter();
            if picked {
                painter.rect_filled(boxed, 4.0, self.accent);
                let (a, b, c) = (
                    boxed.left_center() + egui::vec2(3.5, 0.5),
                    boxed.center_bottom() + egui::vec2(-1.0, -3.5),
                    boxed.right_top() + egui::vec2(-3.0, 4.0),
                );
                painter.line_segment([a, b], egui::Stroke::new(2.0_f32, style::BG));
                painter.line_segment([b, c], egui::Stroke::new(2.0_f32, style::BG));
            } else {
                painter.rect_stroke(
                    boxed,
                    4.0,
                    egui::Stroke::new(1.5_f32, style::CREAM_DIM),
                    egui::StrokeKind::Inside,
                );
            }
        }
        let seed = format!("{}{}", track.artist, track.album);
        let art = egui::Rect::from_min_size(
            rect.left_center() + egui::vec2(6.0 + lead, -16.0),
            egui::vec2(32.0, 32.0),
        );
        self.art.paint(ui, art, track.artwork.as_deref(), &seed);
        let painter = ui.painter();
        let text_left = art.right() + 12.0;
        let text = egui::Rect::from_min_max(
            egui::pos2(text_left, rect.top()),
            egui::pos2(rect.right() - 110.0, rect.bottom()),
        );
        let clip = painter.with_clip_rect(text);
        let (title_color, hint) = if selected {
            (self.accent, Color32::from_white_alpha(150))
        } else {
            (style::CREAM, style::CREAM_DIM)
        };
        clip.text(
            egui::pos2(text_left, rect.top() + 12.0),
            egui::Align2::LEFT_CENTER,
            &track.title,
            egui::FontId::proportional(15.0),
            title_color,
        );
        let detail = format!(
            "{}  -  {}",
            display(&track.artist, "Unknown artist"),
            display(&track.album, "Unknown album")
        );
        clip.text(
            egui::pos2(text_left, rect.top() + 29.0),
            egui::Align2::LEFT_CENTER,
            detail,
            egui::FontId::proportional(12.0),
            hint,
        );
        painter.text(
            egui::pos2(rect.right() - 74.0, rect.center().y),
            egui::Align2::RIGHT_CENTER,
            time(track.duration),
            egui::FontId::proportional(13.0),
            style::CREAM_DIM,
        );
        let edit = egui::Rect::from_center_size(
            egui::pos2(rect.right() - 34.0, rect.center().y),
            egui::vec2(48.0, 26.0),
        );
        let edit_response = ui
            .interact(edit, ui.id().with(("edit", id)), egui::Sense::click())
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        if response.hovered() || edit_response.hovered() {
            let color = if edit_response.hovered() {
                self.accent
            } else {
                style::CREAM_DIM
            };
            ui.painter().text(
                edit.center(),
                egui::Align2::CENTER_CENTER,
                "Edit",
                egui::FontId::proportional(13.0),
                color,
            );
        }
        if edit_response.clicked() {
            self.action = Some(Action::Edit(id));
        }
        if picking && response.clicked() && !self.selected.remove(&id) {
            self.selected.insert(id);
        }
        let activated = if picking {
            false
        } else if self.state.double_click {
            response.double_clicked()
        } else {
            response.clicked()
        };
        if activated {
            self.action = Some(if let Some(i) = queue_index {
                Action::Select(i)
            } else {
                Action::StartVisible(context_index)
            });
        }
        response.context_menu(|ui| {
            if ui.button("Play").clicked() {
                self.action = Some(if let Some(i) = queue_index {
                    Action::Select(i)
                } else {
                    Action::StartVisible(context_index)
                });
                ui.close();
            }
            if ui.button("Add to queue").clicked() {
                self.action = Some(Action::Enqueue(id));
                ui.close();
            }
            if ui.button("Edit tags / filename").clicked() {
                self.action = Some(Action::Edit(id));
                ui.close();
            }
            if ui.button("Find cover online...").clicked() {
                self.action = Some(Action::FindCover(vec![id]));
                ui.close();
            }
        });
    }

    fn library_ui(&mut self, ui: &mut egui::Ui) {
        if self.view == View::Downloads {
            egui::CentralPanel::default().show_inside(ui, |ui| self.download_ui(ui));
            return;
        }
        egui::CentralPanel::default().show_inside(ui, |ui| {
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                ui.heading(self.view.title());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let search = ui.add(egui::TextEdit::singleline(&mut self.search).id(SEARCH_ID.into()).hint_text("Search your music  (Ctrl+F)").desired_width(280.0));
                    if search.changed() { self.rebuild(); }
                    if search.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                        self.search.clear();
                        search.surrender_focus();
                        self.rebuild();
                    }
                });
            });
            let old = (self.sort, self.descending);
            ui.horizontal(|ui| {
                egui::ComboBox::from_id_salt("sort").selected_text(self.sort.title()).show_ui(ui, |ui| {
                    for sort in [Sort::Album, Sort::Artist, Sort::Title, Sort::Year, Sort::Filename] { ui.selectable_value(&mut self.sort, sort, sort.title()); }
                });
                ui.checkbox(&mut self.descending, "Descending");
                ui.label(RichText::new(if self.state.double_click { "Double-click a track to play. Right-click for queue options." } else { "Click a track to play. Right-click for queue options." }).small().weak());
            });
            if old != (self.sort, self.descending) { self.rebuild(); }
            ui.separator();
            if self.tracks.is_empty() {
                ui.add_space(65.0); ui.heading("Start with a music folder");
                ui.label("Choose a folder in the sidebar. Skratch reads its music and album covers, then watches for changes.");
                ui.label("Your files stay where they are. MP4 files play their audio.");
                return;
            }
            if self.view == View::Queue {
                ui.horizontal(|ui| {
                    ui.label(format!("{} queued tracks", self.state.queue.ids.len()));
                    if ui.button("Clear queue").clicked() { self.stop(); self.state.queue.start(Vec::new(), 0); }
                });
                self.select_bar(ui, self.state.queue.ids.clone());
                let indices: Vec<(usize, usize)> = self.state.queue.ids.iter().enumerate().filter_map(|(q, id)| self.by_id.get(id).map(|i| (q, *i))).collect();
                egui::ScrollArea::vertical().id_salt("queue").show_rows(ui, 40.0, indices.len(), |ui, range| {
                    for r in range { let (q, i) = indices[r]; self.track_row(ui, i, Some(q), 0); }
                });
                return;
            }
            if matches!(self.view, View::Albums | View::Artists) {
                if let Some(group_index) = self.selected_group {
                    if ui.button("< Back").clicked() { self.selected_group = None; return; }
                    let group = &self.groups[group_index];
                    ui.heading(&group.name); ui.label(&group.subtitle);
                    let count = group.indices.len();
                    let ids: Vec<i64> = self.groups[group_index].indices.iter().map(|i| self.tracks[*i].id).collect();
                    ui.horizontal(|ui| {
                        if ui.button("Play all").clicked() { self.action = Some(Action::StartVisible(0)); }
                        if ui.button("Find cover online...").clicked() { self.action = Some(Action::FindCover(ids.clone())); }
                    });
                    self.select_bar(ui, ids);
                    egui::ScrollArea::vertical().id_salt("group-tracks").show_rows(ui, 40.0, count, |ui, range| {
                        for r in range { let i = self.groups[group_index].indices[r]; self.track_row(ui, i, None, r); }
                    });
                } else {
                    let missing: Vec<i64> = self.filtered.iter().map(|i| &self.tracks[*i]).filter(|t| t.artwork.is_none()).map(|t| t.id).collect();
                    if self.view == View::Albums && !missing.is_empty() {
                        let label = format!("Find covers for the {} tracks without one...", missing.len());
                        if ui.button(label).clicked() { self.action = Some(Action::FindCover(missing)); }
                    }
                    let columns = ((ui.available_width() / 180.0) as usize).max(1);
                    let rows = self.groups.len().div_ceil(columns);
                    egui::ScrollArea::vertical().id_salt("group-grid").show_rows(ui, 230.0, rows, |ui, range| {
                        for row in range {
                            ui.horizontal(|ui| {
                                for col in 0..columns {
                                    let index = row * columns + col;
                                    let Some(group) = self.groups.get(index) else { break; };
                                    ui.allocate_ui_with_layout(egui::vec2(170.0, 222.0), egui::Layout::top_down(egui::Align::LEFT), |ui| {
                                        const SIZE: f32 = 145.0;
                                        ui.set_min_size(egui::vec2(170.0, 222.0));
                                        let seed = format!("{}{}", group.name, group.subtitle);
                                        let origin = ui.cursor().min;
                                        let sleeve = egui::Rect::from_min_size(origin, egui::vec2(SIZE, SIZE));
                                        let peek = ui.ctx().animate_bool_with_time(ui.id().with(("peek", index)), ui.rect_contains_pointer(sleeve), 0.18);
                                        if peek > 0.01 {
                                            // the record slides out of the sleeve on hover
                                            let center = egui::pos2(origin.x + SIZE * 0.5 + 22.0 * peek, origin.y + SIZE * 0.5);
                                            style::record(ui.painter(), center, SIZE * 0.47, None, style::seed_color(&seed), 0.0);
                                        }
                                        let cover = self.art.show(ui, group.indices.first().and_then(|i| self.tracks[*i].artwork.as_deref()), SIZE, &seed).on_hover_cursor(egui::CursorIcon::PointingHand);
                                        let title = ui.add(egui::Label::new(RichText::new(&group.name).strong().size(14.5).color(style::CREAM)).truncate().sense(egui::Sense::click())).on_hover_text(&group.name);
                                        if cover.clicked() || title.clicked() { self.selected_group = Some(index); }
                                        ui.add(egui::Label::new(RichText::new(&group.subtitle).size(12.5).color(style::CREAM_DIM)).truncate());
                                        ui.label(RichText::new(format!("{} track{}", group.indices.len(), if group.indices.len() == 1 { "" } else { "s" })).small().weak());
                                    });
                                }
                            });
                        }
                    });
                }
            } else {
                ui.horizontal(|ui| {
                    ui.label(format!("{} matching tracks", self.filtered.len()));
                    if ui.add_enabled(!self.filtered.is_empty(), egui::Button::new("Play all")).clicked() { self.action = Some(Action::StartVisible(0)); }
                });
                let ids = self.filtered.iter().map(|i| self.tracks[*i].id).collect();
                self.select_bar(ui, ids);
                egui::ScrollArea::vertical().id_salt("all-tracks").show_rows(ui, 40.0, self.filtered.len(), |ui, range| {
                    for r in range { let i = self.filtered[r]; self.track_row(ui, i, None, r); }
                });
            }
        });
    }

    fn download_ui(&mut self, ui: &mut egui::Ui) {
        ui.add_space(10.0);
        ui.heading("Downloads");
        ui.add_space(12.0);
        ui.label("Link");
        ui.add(
            egui::TextEdit::singleline(&mut self.download_url)
                .hint_text("https://...")
                .desired_width(f32::INFINITY),
        );
        ui.add_space(6.0);
        if self
            .state
            .download_source
            .as_ref()
            .is_none_or(|path| !self.sources.contains(path))
        {
            self.state.download_source = self.sources.first().cloned();
        }
        ui.label("Save to music folder");
        egui::ComboBox::from_id_salt("download-folder")
            .width(ui.available_width().min(560.0))
            .selected_text(
                self.state
                    .download_source
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| "Add a music folder first".into()),
            )
            .show_ui(ui, |ui| {
                for path in &self.sources {
                    ui.selectable_value(
                        &mut self.state.download_source,
                        Some(path.clone()),
                        path.display().to_string(),
                    );
                }
            });
        ui.add_space(6.0);
        ui.label("Audio format");
        egui::ComboBox::from_id_salt("download-format")
            .selected_text(match self.state.download_format {
                DownloadFormat::Best => "Original audio",
                DownloadFormat::Mp3 => "MP3",
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(
                    &mut self.state.download_format,
                    DownloadFormat::Best,
                    "Original audio",
                );
                ui.selectable_value(&mut self.state.download_format, DownloadFormat::Mp3, "MP3");
            });
        ui.label(egui::RichText::new("Original audio keeps the source codec where possible. Artwork and tags are included when available.").small().weak());
        ui.add_space(10.0);
        let running = self.downloader.is_running();
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    !running
                        && self.state.download_source.is_some()
                        && !self.download_url.trim().is_empty(),
                    egui::Button::new("Download audio"),
                )
                .clicked()
            {
                let request = DownloadRequest {
                    url: self.download_url.trim().to_owned(),
                    destination: self.state.download_source.clone().unwrap(),
                    format: self.state.download_format,
                };
                self.download_log.clear();
                match self.downloader.start(request) {
                    Ok(()) => self.download_status = "Starting download...".into(),
                    Err(e) => self.download_status = format!("Cannot start download: {e:#}"),
                }
            }
            if ui
                .add_enabled(running, egui::Button::new("Cancel"))
                .clicked()
            {
                self.downloader.cancel();
                self.download_status = "Cancelling...".into();
            }
            if running {
                ui.spinner();
            }
        });
        ui.add_space(12.0);
        if !self.download_status.is_empty() {
            ui.label(&self.download_status);
        }
        ui.label(
            egui::RichText::new(
                "One item per link. Completed downloads appear in the library automatically.",
            )
            .small()
            .weak(),
        );
        ui.add_space(8.0);
        egui::CollapsingHeader::new("Download details").show(ui, |ui| {
            egui::ScrollArea::vertical()
                .id_salt("download-log")
                .max_height(260.0)
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    for line in &self.download_log {
                        ui.label(egui::RichText::new(line).monospace().small());
                    }
                });
        });
    }

    /// Tracks of one album share a cover; tracks without an album each get their own.
    fn cover_items(&self, ids: &[i64]) -> VecDeque<CoverItem> {
        let mut items: VecDeque<CoverItem> = VecDeque::new();
        let mut keys: Vec<Option<(String, String)>> = Vec::new();
        for track in ids
            .iter()
            .filter_map(|id| self.by_id.get(id))
            .map(|i| &self.tracks[*i])
        {
            let artist = display(&track.album_artist, &track.artist);
            let key = (!track.album.is_empty())
                .then(|| (artist.to_lowercase(), track.album.to_lowercase()));
            if key.is_some()
                && let Some(at) = keys.iter().position(|k| *k == key)
            {
                items[at].ids.push(track.id);
                continue;
            }
            let query = if track.album.is_empty() {
                track
                    .title
                    .split(['(', '['])
                    .next()
                    .unwrap_or_default()
                    .trim()
                    .to_owned()
            } else {
                track.album.clone()
            };
            keys.push(key);
            items.push_back(CoverItem {
                ids: vec![track.id],
                artist: artist.to_owned(),
                query,
            });
        }
        items
    }

    /// Opens the cover search for `ids`, one album at a time, and searches the first right away.
    fn open_cover(&mut self, ids: Vec<i64>) {
        let mut rest = self.cover_items(&ids);
        let Some(first) = rest.pop_front() else {
            return;
        };
        let mut cover = CoverSearch {
            total: rest.len() + 1,
            rest,
            ids: Vec::new(),
            artist: String::new(),
            query: String::new(),
            found: Vec::new(),
            chosen: None,
            status: String::new(),
            working: false,
            rx: None,
        };
        cover.begin(first, self.wake.clone());
        self.cover = Some(cover);
    }

    fn cover_window(&mut self, ctx: &egui::Context) {
        let wake = self.wake.clone();
        let Some(cover) = self.cover.as_mut() else {
            return;
        };
        let mut messages = Vec::new();
        while let Some(message) = cover.rx.as_ref().and_then(|rx| rx.try_recv().ok()) {
            messages.push(message);
        }
        let mut downloaded = None;
        for message in messages {
            match message {
                CoverMsg::Found(Ok(list)) => {
                    cover.working = false;
                    cover.status = if list.is_empty() {
                        "Nothing found. Try fewer words.".into()
                    } else {
                        String::new()
                    };
                    cover.found = list
                        .into_iter()
                        .take(24)
                        .map(|candidate| Found {
                            candidate,
                            texture: None,
                            failed: false,
                        })
                        .collect();
                }
                CoverMsg::Found(Err(error)) => {
                    cover.working = false;
                    cover.status = format!("Search failed: {error}");
                }
                CoverMsg::Thumb(i, image) => {
                    if let Some(found) = cover.found.get_mut(i) {
                        match image {
                            Some(image) => {
                                found.texture = Some(ctx.load_texture(
                                    format!("cover-candidate-{i}"),
                                    image,
                                    egui::TextureOptions::LINEAR,
                                ))
                            }
                            None => found.failed = true,
                        }
                    }
                }
                CoverMsg::Full(Ok(bytes)) => downloaded = Some(bytes),
                CoverMsg::Full(Err(error)) => {
                    cover.working = false;
                    cover.status = format!("Could not download that cover: {error}");
                }
            }
        }
        let embed = &mut self.state.embed_cover;
        let accent = self.accent;
        let (mut open, mut search, mut apply, mut cancel, mut skip) =
            (true, false, false, false, false);
        egui::Window::new("Find cover online")
            .open(&mut open)
            .default_pos(ctx.content_rect().center() - egui::vec2(350.0, 250.0))
            .collapsible(false)
            .default_width(700.0)
            .show(ctx, |ui| {
                let count = cover.ids.len();
                let step = if cover.total > 1 { format!("Cover {} of {}  -  ", cover.total - cover.rest.len(), cover.total) } else { String::new() };
                ui.label(RichText::new(format!("{step}{count} track{}. Double-click a cover to use it.", if count == 1 { "" } else { "s" })).small().weak());
                ui.horizontal(|ui| {
                    ui.label("Artist");
                    let a = ui.add(egui::TextEdit::singleline(&mut cover.artist).desired_width(190.0));
                    ui.label("Album or title");
                    let b = ui.add(egui::TextEdit::singleline(&mut cover.query).desired_width(250.0));
                    let enter = (a.lost_focus() || b.lost_focus()) && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if (ui.add_enabled(!cover.working, egui::Button::new("Search")).clicked() || enter) && !cover.working {
                        search = true;
                    }
                });
                if !cover.status.is_empty() {
                    ui.label(RichText::new(&cover.status).color(if cover.working { style::CREAM_DIM } else { accent }));
                }
                egui::ScrollArea::vertical().max_height(410.0).show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = egui::vec2(10.0, 10.0);
                        for (i, found) in cover.found.iter().enumerate().filter(|(_, f)| !f.failed) {
                            let (rect, response) = ui.allocate_exact_size(egui::vec2(132.0, 190.0), egui::Sense::click());
                            let art = egui::Rect::from_min_size(rect.min + egui::vec2(2.0, 2.0), egui::vec2(128.0, 128.0));
                            match &found.texture {
                                Some(texture) => egui::Image::new((texture.id(), art.size())).corner_radius(6.0).paint_at(ui, art),
                                None => { ui.painter().rect_filled(art, 6.0, style::RAISED); }
                            }
                            let painter = ui.painter();
                            if cover.chosen == Some(i) {
                                painter.rect_stroke(rect, 8.0, egui::Stroke::new(2.0_f32, accent), egui::StrokeKind::Inside);
                            } else if response.hovered() {
                                painter.rect_stroke(rect, 8.0, egui::Stroke::new(1.0_f32, style::CREAM_DIM), egui::StrokeKind::Inside);
                            }
                            let text = painter.with_clip_rect(egui::Rect::from_min_size(rect.min + egui::vec2(0.0, 134.0), egui::vec2(132.0, 34.0)));
                            let label = painter.layout(found.candidate.label.clone(), egui::FontId::proportional(12.0), style::CREAM, 124.0);
                            text.galley(rect.min + egui::vec2(4.0, 137.0), label, style::CREAM);
                            painter.text(rect.min + egui::vec2(4.0, 178.0), egui::Align2::LEFT_CENTER, found.candidate.source, egui::FontId::proportional(11.0), style::CREAM_DIM);
                            let response = response.on_hover_text(&found.candidate.label);
                            if response.clicked() {
                                cover.chosen = Some(i);
                            }
                            if response.double_clicked() && !cover.working {
                                apply = true;
                            }
                        }
                    });
                });
                ui.separator();
                ui.checkbox(embed, "Also save into the audio file");
                ui.label(RichText::new(if *embed {
                    "The cover is written into each audio file, so other players show it too. Files are rewritten safely through a temporary copy."
                } else {
                    "The cover stays in Skratch only. Your audio files are not touched, and it survives rescans."
                }).small().weak());
                ui.horizontal(|ui| {
                    let more = !cover.rest.is_empty();
                    if ui.add_enabled(cover.chosen.is_some() && !cover.working, egui::Button::new(if more { "Use this cover, next" } else { "Use this cover" })).clicked() {
                        apply = true;
                    }
                    if more && ui.button("Skip").clicked() {
                        skip = true;
                    }
                    if ui.button(if cover.total > 1 { "Close" } else { "Cancel" }).clicked() {
                        cancel = true;
                    }
                });
            });
        if cancel {
            open = false;
        }
        if search {
            cover.rx = Some(spawn_cover_search(
                cover.artist.clone(),
                cover.query.clone(),
                wake.clone(),
            ));
            cover.found.clear();
            cover.chosen = None;
            cover.working = true;
            cover.status = "Searching...".into();
        } else if apply && let Some(found) = cover.chosen.and_then(|i| cover.found.get(i)) {
            let url = found.candidate.full.clone();
            let (tx, rx) = mpsc::channel();
            let wake = wake.clone();
            std::thread::spawn(move || {
                let _ = tx.send(CoverMsg::Full(
                    online::fetch_image(&url).map_err(|e| format!("{e:#}")),
                ));
                wake();
            });
            cover.rx = Some(rx);
            cover.working = true;
            cover.status = "Downloading cover...".into();
        }
        let send = downloaded.map(|bytes| Command::SetCover(cover.ids.clone(), bytes, *embed));
        let mut finished = !open;
        if send.is_some() || skip {
            match cover.rest.pop_front() {
                Some(item) => cover.begin(item, wake),
                None => finished = true,
            }
        }
        if finished {
            self.cover = None;
        }
        if let Some(command) = send {
            self.command(command);
        }
    }

    fn edit_window(&mut self, ctx: &egui::Context) {
        let Some(editor) = self.editor.as_mut() else {
            return;
        };
        let mut open = true;
        let mut save = false;
        let mut find_cover = false;
        egui::Window::new("Edit tags and filename").open(&mut open).collapsible(false).resizable(false).default_width(560.0).min_width(520.0).show(ctx, |ui| {
            egui::Grid::new("tag-editor").spacing([12.0, 8.0]).show(ui, |ui| {
                for (label, value) in [
                    ("Title", &mut editor.edit.title), ("Artist", &mut editor.edit.artist),
                    ("Album", &mut editor.edit.album), ("Album artist", &mut editor.edit.album_artist),
                    ("Genre", &mut editor.edit.genre), ("Year", &mut editor.year),
                    ("Track number", &mut editor.track), ("Disc number", &mut editor.disc),
                    ("Filename", &mut editor.edit.filename),
                ] { ui.label(label); ui.add_sized([340.0, 22.0], egui::TextEdit::singleline(value)); ui.end_row(); }
            });
            if ui.button("Find cover online...").clicked() { find_cover = true; }
            ui.label("Save changes to the original file. The folder and file type stay the same.");
            if !editor.error.is_empty() { ui.colored_label(Color32::LIGHT_RED, &editor.error); }
            if ui.add_enabled(!editor.saving, egui::Button::new("Save changes")).clicked() {
                let parse = |text: &str| -> Result<Option<u32>, std::num::ParseIntError> { if text.trim().is_empty() { Ok(None) } else { text.trim().parse().map(Some) } };
                match (parse(&editor.year), parse(&editor.track), parse(&editor.disc)) {
                    (Ok(year), Ok(track), Ok(disc)) => { editor.edit.year = year; editor.edit.track_number = track; editor.edit.disc_number = disc; save = true; },
                    _ => editor.error = "Year, track number, and disc number must be whole positive numbers or blank.".into(),
                }
            }
        });
        if find_cover {
            let id = self.editor.as_ref().map(|editor| editor.id);
            self.open_cover(id.into_iter().collect());
        }
        if save {
            let editor = self.editor.as_mut().unwrap();
            editor.saving = true;
            editor.error.clear();
            let command = Command::Edit(editor.id, editor.edit.clone());
            self.command(command);
        } else if !open {
            self.editor = None;
        }
    }
}

impl eframe::App for Skratch {
    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        self.poll(ctx);
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::F)) {
            if self.view == View::Downloads {
                self.view = View::Albums;
                self.selected_group = None;
                self.rebuild();
            }
            ctx.memory_mut(|m| m.request_focus(egui::Id::new(SEARCH_ID)));
        }
        if !ctx.wants_keyboard_input() && ctx.input(|input| input.key_pressed(egui::Key::Space)) {
            self.perform(Action::Pause);
        }
        self.animate(ctx);
        let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
        let radius = if maximized { 0 } else { style::RADIUS };
        let root = Frame::new()
            .fill(style::BG)
            .corner_radius(CornerRadius::same(radius));
        egui::CentralPanel::default().frame(root).show(ctx, |ui| {
            let outline = ui.max_rect();
            self.title_bar(ui, radius);
            if !self.messages.is_empty() {
                egui::TopBottomPanel::top("messages").show_inside(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.colored_label(self.accent, self.messages.last().unwrap());
                        if ui.small_button("Dismiss").clicked() {
                            self.messages.pop();
                        }
                    });
                });
            }
            self.player_bar(ui, radius);
            self.sidebar(ui);
            self.library_ui(ui);
            if !maximized {
                ui.painter().rect_stroke(
                    outline,
                    radius,
                    egui::Stroke::new(1.0_f32, style::EDGE),
                    egui::StrokeKind::Inside,
                );
            }
        });
        self.edit_window(ctx);
        self.cover_window(ctx);
        self.fill_window(ctx);
        style::resize_edges(ctx);
        if let Some(action) = self.action.take() {
            self.perform(action);
        }
        // Sleep between interaction/events, with a low-rate progress tick during playback.
        ctx.request_repaint_after(if self.playing {
            Duration::from_millis(250)
        } else {
            Duration::from_secs(1)
        });
    }

    fn clear_color(&self, _: &egui::Visuals) -> [f32; 4] {
        egui::Rgba::TRANSPARENT.to_array()
    }

    fn on_exit(&mut self, _: Option<&eframe::glow::Context>) {
        self.save();
    }
}

/// Searches online sources, then streams thumbnails back one by one. Dropping the receiver stops it.
fn spawn_cover_search(
    artist: String,
    query: String,
    wake: Arc<dyn Fn() + Send + Sync>,
) -> mpsc::Receiver<CoverMsg> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let send = |message| {
            let delivered = tx.send(message).is_ok();
            wake();
            delivered
        };
        let list = match online::search(&artist, &query) {
            Ok(list) => list,
            Err(error) => {
                send(CoverMsg::Found(Err(format!("{error:#}"))));
                return;
            }
        };
        let thumbs: Vec<String> = list.iter().take(24).map(|c| c.thumb.clone()).collect();
        if !send(CoverMsg::Found(Ok(list))) {
            return;
        }
        for (i, url) in thumbs.into_iter().enumerate() {
            let image = online::fetch_image(&url)
                .ok()
                .and_then(|bytes| image::load_from_memory(&bytes).ok())
                .map(|image| {
                    let image = image.thumbnail(200, 200).to_rgba8();
                    egui::ColorImage::from_rgba_unmultiplied(
                        [image.width() as usize, image.height() as usize],
                        image.as_raw(),
                    )
                });
            if !send(CoverMsg::Thumb(i, image)) {
                return;
            }
        }
    });
    rx
}

/// A sidebar section heading: a divider line (except for the first) and a small caps label.
fn section(ui: &mut egui::Ui, title: &str, divider: bool) {
    if divider {
        ui.add_space(14.0);
        ui.separator();
    }
    ui.label(
        RichText::new(title)
            .size(11.0)
            .strong()
            .color(style::CREAM_DIM),
    );
    ui.add_space(2.0);
}
fn display<'a>(text: &'a str, fallback: &'a str) -> &'a str {
    if text.is_empty() { fallback } else { text }
}
fn time(seconds: f64) -> String {
    let seconds = seconds.max(0.0) as u64;
    if seconds >= 3600 {
        format!(
            "{}:{:02}:{:02}",
            seconds / 3600,
            (seconds / 60) % 60,
            seconds % 60
        )
    } else {
        format!("{}:{:02}", seconds / 60, seconds % 60)
    }
}
