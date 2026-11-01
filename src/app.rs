use crate::{
    artwork::Artwork,
    service::{self, Command, Event},
    state::{NowPanel, Playlist, SavedState, Show, Source},
    style::{self, Icon},
};
use eframe::egui::{self, Color32, CornerRadius, Frame, Margin, RichText};
use skratch::{
    analysis::Reactive,
    download::{DownloadEvent, DownloadFormat, DownloadRequest, Downloader},
    lyrics,
    media::{DesktopControls, MediaCommand},
    model::{MetadataEdit, Track},
    online::{self, Candidate},
    player::{EQ_BANDS, Player, PlayerEvent},
    queue::Repeat,
    tagparse::{self, Parsed},
    tools,
};
use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    path::PathBuf,
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

mod fetch;
mod listen;
mod lyricview;
mod playing;
mod setup;
mod shell;
mod visualizer;

pub use shell::Shell;

const SEARCH_ID: &str = "library-search";

/// Whether the track's title, artist, album, genre or file path contains `query` (lowercase).
fn matches_query(track: &Track, query: &str) -> bool {
    query.is_empty()
        || [
            &track.title,
            &track.artist,
            &track.album,
            &track.album_artist,
            &track.genre,
        ]
        .iter()
        .any(|s| s.to_lowercase().contains(query))
        || track.path.to_string_lossy().to_lowercase().contains(query)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    Tracks,
    Albums,
    Artists,
    Queue,
    Playing,
    Downloads,
    Tidy,
    Playlist(usize),
}
impl View {
    fn icon(self) -> &'static str {
        match self {
            Self::Tracks => "🎵",
            Self::Albums => "💿",
            Self::Artists => "👤",
            Self::Queue => "☰",
            Self::Playing => "▶",
            Self::Downloads => "⬇",
            Self::Tidy => "🔧",
            Self::Playlist(_) => "♫",
        }
    }
    fn title(self) -> &'static str {
        match self {
            Self::Albums => "Albums",
            Self::Artists => "Artists",
            Self::Tracks => "All tracks",
            Self::Queue => "Play queue",
            Self::Playing => "Now playing",
            Self::Downloads => "Downloads",
            Self::Tidy => "Tidy up",
            Self::Playlist(_) => "Playlist",
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
    /// Most recently played first (with Descending on).
    Recent,
    Plays,
    Rating,
}
impl Sort {
    fn title(self) -> &'static str {
        match self {
            Self::Album => "Album / track",
            Self::Artist => "Artist",
            Self::Title => "Title",
            Self::Year => "Year",
            Self::Filename => "Filename",
            Self::Recent => "Last played",
            Self::Plays => "Play count",
            Self::Rating => "Rating",
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
    /// The file dialog was closed without choosing anything.
    Cancelled,
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
    /// Play this track from the whole library, in sort order.
    StartLibrary(i64),
    /// Play this track from what the queue came from (the Now playing library panel).
    StartScoped(i64),
    /// Play the open playlist from this row.
    StartPlaylist(usize),
    Select(usize),
    Edit(i64),
    Pause,
    Next,
    Previous,
    Stop,
    Seek(f64),
    Volume(f64),
    Eq([f32; 10]),
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
    /// Player events set aside while a new song was loading, handled before newer ones.
    replay: VecDeque<PlayerEvent>,
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
    busy_since: Option<std::time::Instant>,
    messages: Vec<String>,
    editor: Option<Editor>,
    source_text: String,
    folder_rx: Option<mpsc::Receiver<Option<PathBuf>>>,
    action: Option<Action>,
    last_save: Instant,
    last_desktop: Instant,
    downloader: Downloader,
    data_dir: PathBuf,
    tools: tools::Status,
    setup: Option<setup::Setup>,
    download_url: String,
    download_status: String,
    download_log: std::collections::VecDeque<String>,
    select_mode: bool,
    selected: HashSet<i64>,
    /// The row last clicked, where a shift-click range starts.
    anchor: Option<i64>,
    /// The track ids in the list being shown this frame, in order.
    visible: Vec<i64>,
    /// A cover image being chosen for every selected track.
    pick_rx: Option<mpsc::Receiver<Result<Option<Vec<u8>>, String>>>,
    fill_open: bool,
    cover: Option<CoverSearch>,
    fetch: Option<fetch::TagFetch>,
    logo: eframe::egui::TextureHandle,
    accent: Color32,
    accent_applied: Color32,
    /// The page colour last given to egui, which follows the accent when `look.mood` is on.
    panel_applied: Color32,
    fonts_applied: bool,
    grain: egui::TextureHandle,
    spin: f32,
    spin_speed: f32,
    arm: f32,
    arm_velocity: f32,
    /// The seek bar is being dragged: the arm lifts and the record follows the pointer.
    scrubbing: bool,
    /// The smoothed bass level of what is playing, 0 to 1.
    beat: f32,
    /// The view and group last drawn, and when it appeared, for the page fade-in.
    shown: (View, Option<usize>),
    shown_at: f64,
    /// `page_fade` for this frame.
    fade: f32,
    /// The cover being shown, the one before it, and when it changed, for the crossfade.
    shown_art: Option<PathBuf>,
    prev_art: Option<PathBuf>,
    art_changed_at: f64,
    reactive: Reactive,
    viz: visualizer::Viz,
    lyrics: lyrics::Lookup,
    /// The track the Now playing list was last scrolled to, so it follows track changes.
    listed: Option<i64>,
    /// Until this time (egui seconds) a track change counts as caused by a click in that list.
    list_hold: f64,
    /// The Now playing view last drawn and when it appeared, for the animation between views.
    panel_shown: Option<NowPanel>,
    panel_shown_at: f64,
    /// How far that animation has got this frame.
    reveal: playing::Reveal,
    drawer: playing::Drawer,
    /// When the last frame began. The watcher thread steps in when frames stop coming (a
    /// minimised window gets none on Wayland), so playback and the media keys keep working.
    frame_at: Instant,
    listen: listen::Listen,
}

impl Skratch {
    pub fn new(cc: &eframe::CreationContext<'_>, data_dir: PathBuf) -> Self {
        let ctx = cc.egui_ctx.clone();
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(move || ctx.request_repaint());
        style::apply(&cc.egui_ctx, style::ACCENT, style::PANEL);
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
        let fonts_applied = state.look.fonts;
        style::fonts(&cc.egui_ctx, fonts_applied);
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
        let setup = (!state.setup_done).then(|| setup::Setup::new(false));
        let mut app = Self {
            service: service::Service::start(data_dir.clone(), wake.clone()),
            tracks: Vec::new(),
            by_id: HashMap::new(),
            sources: Vec::new(),
            state,
            state_path,
            player: None,
            desktop,
            downloader: Downloader::new(wake.clone()),
            data_dir,
            tools: tools::Status::default(),
            setup,
            download_url: String::new(),
            download_status: String::new(),
            download_log: std::collections::VecDeque::new(),
            wake,
            playing: false,
            loaded: false,
            pending_seek: None,
            replay: VecDeque::new(),
            duration: 0.0,
            view: View::Tracks,
            sort: Sort::Album,
            descending: false,
            search: String::new(),
            filtered: Vec::new(),
            groups: Vec::new(),
            selected_group: None,
            art: Artwork::new(cc.egui_ctx.clone()),
            busy: false,
            busy_since: None,
            messages,
            editor: None,
            source_text: String::new(),
            folder_rx: None,
            action: None,
            last_save: Instant::now(),
            last_desktop: Instant::now(),
            select_mode: false,
            selected: HashSet::new(),
            anchor: None,
            visible: Vec::new(),
            pick_rx: None,
            fill_open: false,
            cover: None,
            fetch: None,
            logo: style::load_logo(&cc.egui_ctx),
            accent: style::ACCENT,
            accent_applied: style::ACCENT,
            panel_applied: style::PANEL,
            fonts_applied,
            grain: style::grain_texture(&cc.egui_ctx),
            spin: 0.0,
            spin_speed: 0.0,
            arm: 0.0,
            arm_velocity: 0.0,
            scrubbing: false,
            beat: 0.0,
            shown: (View::Tracks, None),
            shown_at: -10.0,
            fade: 1.0,
            shown_art: None,
            prev_art: None,
            art_changed_at: -10.0,
            reactive: Reactive::default(),
            viz: Default::default(),
            lyrics: lyrics::Lookup::default(),
            listed: None,
            list_hold: 0.0,
            panel_shown: None,
            panel_shown_at: 0.0,
            reveal: playing::Reveal::DONE,
            drawer: Default::default(),
            frame_at: Instant::now(),
            listen: Default::default(),
        };
        app.refresh_tools();
        app
    }

    /// `base` with a hint of the cover's colour, unless the mood tint is off.
    fn mood(&self, base: Color32, amount: f32) -> Color32 {
        if self.state.look.mood {
            style::tint(base, self.accent, amount)
        } else {
            base
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

    /// Sorts track indices by the chosen column.
    fn sort_indices(&self, indices: &mut [usize]) {
        // the lowercase text to compare, made once per track instead of once per comparison
        let mut keyed: Vec<(usize, String, String)> = indices
            .iter()
            .map(|&i| {
                let t = &self.tracks[i];
                let (first, second) = match self.sort {
                    Sort::Album => (t.album.to_lowercase(), t.album_artist.to_lowercase()),
                    Sort::Artist => (t.artist.to_lowercase(), t.album.to_lowercase()),
                    Sort::Title => (t.title.to_lowercase(), String::new()),
                    Sort::Year => (t.album.to_lowercase(), String::new()),
                    Sort::Filename | Sort::Recent | Sort::Plays | Sort::Rating => {
                        (String::new(), String::new())
                    }
                };
                (i, first, second)
            })
            .collect();
        keyed.sort_by(|(a, a1, a2), (b, b1, b2)| {
            let a = &self.tracks[*a];
            let b = &self.tracks[*b];
            let cmp = match self.sort {
                Sort::Album | Sort::Artist => a1.cmp(b1).then_with(|| a2.cmp(b2)),
                Sort::Title => a1.cmp(b1),
                Sort::Year => a.year.cmp(&b.year).then_with(|| a1.cmp(b1)),
                Sort::Filename => a.path.cmp(&b.path),
                Sort::Recent => a.last_played.cmp(&b.last_played),
                Sort::Plays => a.plays.cmp(&b.plays),
                Sort::Rating => a.rating.cmp(&b.rating),
            }
            .then_with(|| a.disc_number.cmp(&b.disc_number))
            .then_with(|| a.track_number.cmp(&b.track_number))
            .then_with(|| a.path.cmp(&b.path));
            if self.descending { cmp.reverse() } else { cmp }
        });
        for (slot, (i, ..)) in indices.iter_mut().zip(keyed) {
            *slot = i;
        }
    }

    fn start_action(&self, id: i64, queue_index: Option<usize>, context_index: usize) -> Action {
        if let Some(q) = queue_index {
            Action::Select(q)
        } else if matches!(self.view, View::Playlist(_)) {
            Action::StartPlaylist(context_index)
        } else if self.view == View::Playing {
            Action::StartScoped(id)
        } else if self.selected_group.is_none() {
            Action::StartLibrary(id)
        } else {
            Action::StartVisible(context_index)
        }
    }

    /// Every track id, in the library's sort order, ignoring any search.
    fn library_ids(&self) -> Vec<i64> {
        let mut all: Vec<usize> = (0..self.tracks.len()).collect();
        self.sort_indices(&mut all);
        all.into_iter().map(|i| self.tracks[i].id).collect()
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
            .filter(|(_, t)| self.state.show.keeps(t) && matches_query(t, &query))
            .map(|(i, _)| i)
            .collect();
        let mut filtered = std::mem::take(&mut self.filtered);
        self.sort_indices(&mut filtered);
        self.filtered = filtered;
        // Names that differ only in capitalisation are one group, shown as first spelled.
        let mut groups: BTreeMap<(String, String), Group> = BTreeMap::new();
        for i in &self.filtered {
            let t = &self.tracks[*i];
            let (name, subtitle) = if self.view == View::Artists {
                (display(&t.artist, "Unknown artist"), "")
            } else {
                (
                    display(&t.album, "Unknown album"),
                    display(
                        if t.album_artist.is_empty() {
                            &t.artist
                        } else {
                            &t.album_artist
                        },
                        "Unknown artist",
                    ),
                )
            };
            groups
                .entry((name.to_lowercase(), subtitle.to_lowercase()))
                .or_insert_with(|| Group {
                    name: name.to_owned(),
                    subtitle: subtitle.to_owned(),
                    indices: Vec::new(),
                })
                .indices
                .push(*i);
        }
        self.groups = groups.into_values().collect();
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
                (g.name.to_lowercase(), g.subtitle.to_lowercase())
                    == (key.0.to_lowercase(), key.1.to_lowercase())
            })
        });
    }

    fn start_current(&mut self, position: f64) {
        let Some(track) = self.current().cloned() else {
            return;
        };
        if self.player.is_none() {
            match Player::start(self.wake.clone()) {
                Ok(player) => {
                    self.player = Some(player);
                    self.listen.new_player();
                }
                Err(e) => {
                    self.message(format!("Cannot start playback: {e:#}"));
                    return;
                }
            }
        }
        let player = self.player.as_mut().unwrap();
        // a fade-in starts silent, so there is no burst before the first update
        let gain = listen::fade_gain(self.state.fade_secs, position, track.duration);
        let result = player
            .set_volume(self.state.volume)
            .and_then(|_| player.set_eq(&self.state.eq))
            .and_then(|_| player.set_loudness(self.state.loudness))
            .and_then(|_| player.set_fade(gain))
            .and_then(|_| player.load(&track.path));
        match result {
            Ok(()) => {
                // an end-of-file from the song just replaced may still be queued; it is not
                // this song's, so it must not move the queue on
                while let Ok(event) = player.events.try_recv() {
                    if !matches!(event, PlayerEvent::Ended) {
                        self.replay.push_back(event);
                    }
                }
                self.listen.restart();
                self.listen.gain = gain;
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

    /// Starts the queue from `source` at track `id`, falling back to the library when the
    /// track is not part of it.
    fn start_from(&mut self, id: i64, source: Source) {
        let (source, ids) = match source {
            Source::Tracks(ids) if ids.contains(&id) => (Source::Tracks(ids.clone()), ids),
            _ => (Source::Library, self.library_ids()),
        };
        let index = ids.iter().position(|v| *v == id).unwrap_or(0);
        self.state.source = source;
        self.state.queue.start(ids, index);
        self.start_current(0.0);
    }

    fn perform(&mut self, action: Action) {
        let result = match action {
            Action::StartVisible(index) => {
                let indices = self
                    .selected_group
                    .and_then(|i| self.groups.get(i))
                    .map(|g| &g.indices)
                    .unwrap_or(&self.filtered);
                let ids: Vec<i64> = indices.iter().map(|i| self.tracks[*i].id).collect();
                self.state.source = Source::Tracks(ids.clone());
                self.state.queue.start(ids, index);
                self.start_current(0.0);
                Ok(())
            }
            Action::StartLibrary(id) => {
                self.start_from(id, Source::Library);
                Ok(())
            }
            Action::StartPlaylist(row) => {
                if let View::Playlist(p) = self.view {
                    let ids = self.playlist_ids(p);
                    self.state.source = Source::Tracks(ids.clone());
                    self.state.queue.start(ids, row);
                    self.start_current(0.0);
                }
                Ok(())
            }
            Action::StartScoped(id) => {
                self.start_from(id, self.state.source.clone());
                Ok(())
            }
            Action::FindCover(ids) => {
                self.open_cover(ids);
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
            Action::Eq(gains) => {
                self.state.eq = gains;
                match self.player.as_mut() {
                    Some(player) => player.set_eq(&gains),
                    None => Ok(()),
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
        self.drain_events();
        self.keep_playing(ctx);
        self.art.drain(ctx);
    }

    /// Takes in what the library worker, the downloader and the folder picker have sent. Also
    /// run by the watcher while the window is minimised, so snapshots never pile up unread.
    fn drain_events(&mut self) {
        while let Ok(event) = self.service.events.try_recv() {
            match event {
                Event::Busy(value) => {
                    self.busy = value;
                    self.busy_since = value.then(std::time::Instant::now);
                }
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
                    self.art.invalidate_changed();
                    self.listen.tidy = None;
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
    }

    /// Handles what playback is waiting on: events from mpv (a track ending starts the next
    /// one), the desktop's media keys and widget, and the periodic save. Runs every frame, and
    /// also from the watcher thread when frames stop coming. Returns whether something changed
    /// that a window showing the app should redraw for.
    fn keep_playing(&mut self, ctx: &egui::Context) -> bool {
        let mut changed = false;
        loop {
            let event = self
                .replay
                .pop_front()
                .or_else(|| self.player.as_ref().and_then(|p| p.events.try_recv().ok()));
            let Some(event) = event else {
                break;
            };
            changed |= !matches!(event, PlayerEvent::Position(_));
            match event {
                PlayerEvent::Started => {
                    if let Some(position) = self.pending_seek.take() {
                        self.perform(Action::Seek(position));
                    }
                }
                PlayerEvent::Position(position) => {
                    if self.loaded && self.pending_seek.is_none() && position.is_finite() {
                        self.state.position = position.max(0.0);
                        self.listen.pos_at = Instant::now();
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
                    if self.listen.sleep == Some(listen::Sleep::EndOfSong) {
                        self.listen.sleep = None;
                        self.stop();
                    } else if let Some(next) = self.state.queue.advance(true) {
                        // mpv has already moved on to a song lined up for it
                        if self.listen.preloaded == Some(next) {
                            self.adopt_preloaded();
                        } else {
                            self.start_current(0.0);
                        }
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
            changed = true;
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
        self.playback_tick(ctx);
        changed
    }

    fn save(&mut self) {
        if let Err(e) = self.state.save(&self.state_path) {
            self.message(format!("Cannot save playback state: {e:#}"));
        }
    }

    fn pick_folder(&mut self) {
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

    fn sidebar(&mut self, ui: &mut egui::Ui) {
        let frame = Frame::side_top_panel(ui.style())
            .fill(self.mood(style::SIDEBAR, 0.05))
            .inner_margin(Margin::same(14));
        egui::SidePanel::left("library-sidebar")
            .default_width(215.0)
            .width_range(190.0..=300.0)
            .frame(frame)
            .show_inside(ui, |ui| {
                // a short window or screen would otherwise cut the lower entries off
                style::scroll()
                    .id_salt("sidebar-scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| self.sidebar_entries(ui));
            });
    }

    fn sidebar_entries(&mut self, ui: &mut egui::Ui) {
        ui.add_space(8.0);
        section(ui, "BROWSE", false);
        for view in [
            View::Tracks,
            View::Albums,
            View::Artists,
            View::Queue,
            View::Playing,
            View::Downloads,
            View::Tidy,
        ] {
            if ui
                .selectable_label(
                    self.view == view,
                    RichText::new(format!("{}  {}", view.icon(), view.title())).size(16.0),
                )
                .clicked()
            {
                self.view = view;
                self.selected_group = None;
                self.selected.clear();
                self.rebuild();
            }
        }
        section(ui, "PLAYLISTS", true);
        let names: Vec<String> = self
            .state
            .playlists
            .iter()
            .map(|p| p.name.clone())
            .collect();
        for (i, name) in names.iter().enumerate() {
            let name = display(name, "Untitled playlist");
            if ui
                .selectable_label(
                    self.view == View::Playlist(i),
                    RichText::new(format!("{}  {name}", View::Playlist(i).icon())).size(16.0),
                )
                .clicked()
            {
                self.view = View::Playlist(i);
                self.selected_group = None;
                self.selected.clear();
                self.rebuild();
            }
        }
        if ui.small_button("+ New playlist").clicked() {
            self.state.playlists.push(Playlist {
                name: "New playlist".into(),
                ids: Vec::new(),
            });
            self.view = View::Playlist(self.state.playlists.len() - 1);
            self.selected.clear();
            self.rebuild();
        }
        section(ui, "MUSIC FOLDERS", true);
        let mut remove = None;
        style::scroll()
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
            self.pick_folder();
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
        ui.label(RichText::new(format!("{} tracks", self.tracks.len())).color(style::CREAM));
        // Quick background rescans would otherwise flash the spinner and shift the layout.
        let busy_delay = std::time::Duration::from_millis(300);
        let slow = self.busy_since.is_some_and(|t| t.elapsed() > busy_delay);
        if self.busy && !slow {
            ui.ctx().request_repaint_after(busy_delay);
        }
        if slow {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Updating library...");
            });
        }
        if ui
            .add_enabled(!slow, egui::Button::new("Refresh library"))
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
        ui.checkbox(&mut self.state.lyrics_online, "Look up lyrics online")
            .on_hover_text("Missing lyrics are fetched from lrclib.net, which is sent the artist, title, album and length.");
        let mut fallback = !self.state.lyrics_no_fallback;
        ui.add_enabled(
            self.state.lyrics_online,
            egui::Checkbox::new(&mut fallback, "Also try NetEase Cloud Music"),
        )
        .on_hover_text("When lrclib.net has nothing or is down, music.163.com is asked with the artist, title and length.");
        self.state.lyrics_no_fallback = !fallback;
        ui.add_enabled(
            self.state.lyrics_online,
            egui::Checkbox::new(&mut self.state.lyrics_genius, "Also try Genius (plain text)"),
        )
        .on_hover_text("Last, when the others have nothing: genius.com is asked with the artist and title and its song page is read for the words. Plain lyrics only, no timing. Genius's terms do not allow automated use, so this is off until you switch it on.");
        ui.checkbox(&mut self.state.lyrics_embed, "Save lyrics into song files")
            .on_hover_text("Once a song has been played through, and when you look lyrics up for many songs, the lyrics Skratch holds for it are also written into the audio file's own tags. This changes your files, so it is off until you switch it on.");
        ui.collapsing("Playback", |ui| self.playback_settings(ui));
        ui.collapsing("Effects", |ui| {
            let effects = &mut self.state.effects;
            ui.checkbox(&mut effects.animations, "Animations")
                .on_hover_text("Master switch for everything below.");
            ui.add_enabled_ui(effects.animations, |ui| {
                ui.checkbox(&mut effects.needle, "Moving tonearm")
                    .on_hover_text("It drops with a bounce, creeps inward as the song plays and lifts while you seek.");
                ui.checkbox(&mut effects.scratch, "Scratch the record")
                    .on_hover_text("Dragging the seek bar turns the record.");
                ui.checkbox(&mut effects.seek_life, "Lively seek bar")
                    .on_hover_text("Playhead glow, bars that follow the bass, and the time under the pointer.");
                ui.checkbox(&mut effects.transitions, "Transitions")
                    .on_hover_text("Pages and the Now playing views fade in, cards rise in, covers crossfade and play/pause morphs.");
            });
        });
        ui.collapsing("Look", |ui| {
            let look = &mut self.state.look;
            ui.checkbox(&mut look.mood, "Tint with the cover")
                .on_hover_text(
                    "Panels and the page background take a little of the playing cover's colour.",
                );
            ui.checkbox(&mut look.depth, "Shadows and edges")
                .on_hover_text("Soft shadows under covers and a light edge on the player bar.");
            ui.checkbox(&mut look.grain, "Film grain").on_hover_text(
                "A faint noise over everything; it hides banding in dark gradients.",
            );
            ui.checkbox(&mut look.fonts, "DM Sans and DM Serif")
                .on_hover_text("Off: egui's default typeface.");
        });
        if ui.button("Run setup again").clicked() {
            self.open_setup(false);
        }
    }

    /// Eases the record, tonearm and accent colour towards their targets for this frame.
    fn animate(&mut self, ctx: &egui::Context) {
        let dt = ctx.input(|i| i.stable_dt).min(0.1);
        let ease = |value: &mut f32, target: f32, rate: f32| {
            *value += (target - *value) * (1.0 - (-rate * dt).exp());
        };
        let effects = self.state.effects.clone();
        let playing = if self.playing { 1.0 } else { 0.0 };
        let scrubbing = self.scrubbing && effects.scratch();
        ease(&mut self.spin_speed, playing, 2.5);
        // 33 1/3 rpm, but not while the record is in the user's hand
        if !scrubbing {
            self.spin = (self.spin + self.spin_speed * 3.49 * dt) % std::f32::consts::TAU;
        }
        let lowered = if self.playing && !(self.scrubbing && effects.needle()) {
            1.0
        } else {
            0.0
        };
        if effects.needle() {
            // a spring: dropping onto the record overshoots a little and settles, lifting does not
            let (stiffness, damping) = if lowered > 0.5 {
                (140.0, 14.0)
            } else {
                (90.0, 19.0)
            };
            self.arm_velocity +=
                ((lowered - self.arm) * stiffness - self.arm_velocity * damping) * dt;
            self.arm += self.arm_velocity * dt;
        } else {
            self.arm_velocity = 0.0;
            ease(&mut self.arm, playing, 3.0);
        }
        self.scrubbing = false;
        let now = ctx.input(|i| i.time);
        let art_path = self.current().and_then(|t| t.artwork.clone());
        if art_path != self.shown_art {
            self.prev_art = self.shown_art.take();
            self.shown_art = art_path;
            self.art_changed_at = now;
        }
        if now - self.art_changed_at > 0.5 {
            self.prev_art = None;
        } else {
            ctx.request_repaint();
        }
        if let Some(path) = self.current().map(|t| t.path.clone()) {
            self.beat = self
                .reactive
                .level(ctx, &path, self.state.position, self.playing);
        } else {
            self.beat = 0.0;
        }
        let target = match self.current() {
            Some(t) => match t.artwork.as_deref() {
                // Hold the current colour while the cover loads instead of detouring via the seed hue.
                Some(path) if self.art.loading(path) => self.accent,
                path => path
                    .and_then(|path| self.art.accent(path))
                    .unwrap_or_else(|| style::seed_color(&format!("{}{}", t.artist, t.album))),
            },
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
        if self.state.look.fonts != self.fonts_applied {
            self.fonts_applied = self.state.look.fonts;
            style::fonts(ctx, self.fonts_applied);
        }
        let panel = self.mood(style::PANEL, 0.04);
        if self.accent != self.accent_applied || panel != self.panel_applied {
            style::apply(ctx, self.accent, panel);
            self.accent_applied = self.accent;
            self.panel_applied = panel;
            ctx.request_repaint();
        }
        if self.spin_speed > 0.005
            || (self.arm - lowered).abs() > 0.002
            || self.arm_velocity.abs() > 0.002
        {
            ctx.request_repaint();
        }
    }

    fn title_bar(&mut self, ui: &mut egui::Ui, radius: u8) {
        let now = self
            .current()
            .map(|t| format!("{}  -  {}", t.title, display(&t.artist, "Unknown artist")))
            .unwrap_or_default();
        let frame = Frame::new()
            .fill(self.mood(style::TITLE, 0.05))
            .corner_radius(CornerRadius {
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

    /// How far the new cover has faded in (0 to 1), and the previous cover's texture and backdrop
    /// while it is still fading out.
    fn crossfade(
        &mut self,
        ctx: &egui::Context,
    ) -> (f32, Option<egui::TextureId>, Option<egui::TextureId>) {
        if !self.state.effects.transitions() || self.prev_art.is_none() {
            return (1.0, None, None);
        }
        let t = ((ctx.input(|i| i.time) - self.art_changed_at) / 0.4).clamp(0.0, 1.0) as f32;
        let eased = t * t * (3.0 - 2.0 * t);
        let old = self.prev_art.clone();
        let texture = old.as_deref().and_then(|path| self.art.texture(path));
        let glow = old.as_deref().and_then(|path| self.art.glow(path));
        (eased, texture, glow)
    }

    fn player_bar(&mut self, ui: &mut egui::Ui, radius: u8) {
        let track = self.current().cloned();
        if let Some(track) = &track {
            self.reactive.track(ui.ctx(), &track.path);
        }
        let accent = self.accent;
        let art = track
            .as_ref()
            .and_then(|t| t.artwork.as_deref())
            .and_then(|path| self.art.texture(path));
        let glow = track
            .as_ref()
            .and_then(|t| t.artwork.as_deref())
            .and_then(|path| self.art.glow(path));
        let label = track.as_ref().map_or(accent, |t| {
            style::seed_color(&format!("{}{}", t.artist, t.album))
        });
        let (art_mix, old_art, old_glow) = self.crossfade(ui.ctx());
        let progress = (self.state.position / self.duration.max(1.0)).clamp(0.0, 1.0) as f32;
        let frame = Frame::new()
            .fill(self.mood(style::BAR, 0.06))
            .inner_margin(Margin::symmetric(18, 10))
            .corner_radius(CornerRadius {
                nw: 0,
                ne: 0,
                sw: radius,
                se: radius,
            });
        let panel = egui::TopBottomPanel::bottom("player")
            .exact_height(136.0)
            .frame(frame)
            .show_inside(ui, |ui| {
                // the playing cover, shrunk and stretched over the whole bar
                let bar = ui.max_rect().expand2(egui::vec2(18.0, 10.0));
                let round = CornerRadius {
                    nw: 0,
                    ne: 0,
                    sw: radius,
                    se: radius,
                };
                match glow {
                    Some(glow) => {
                        egui::Image::new((glow, bar.size()))
                            .tint(Color32::from_white_alpha((70.0 * art_mix) as u8))
                            .corner_radius(round)
                            .paint_at(ui, bar);
                        if let Some(old) = old_glow {
                            egui::Image::new((old, bar.size()))
                                .tint(Color32::from_white_alpha((70.0 * (1.0 - art_mix)) as u8))
                                .corner_radius(round)
                                .paint_at(ui, bar);
                        }
                    }
                    // no cover: wash the bar with the track's own colour instead
                    None if track.is_some() => {
                        ui.painter()
                            .rect_filled(bar, round, label.gamma_multiply(0.08));
                    }
                    None => {}
                }
                ui.horizontal(|ui| {
                    let (rect, response) =
                        ui.allocate_exact_size(egui::vec2(160.0, 112.0), egui::Sense::click());
                    let response = response
                        .on_hover_cursor(egui::CursorIcon::PointingHand)
                        .on_hover_text("Open Now playing  (right-click for options)");
                    if response.clicked() {
                        self.view = View::Playing;
                        self.selected_group = None;
                        self.selected.clear();
                        self.rebuild();
                    }
                    if let Some(track) = &track {
                        egui::Popup::context_menu(&response)
                            .show(|ui| self.track_actions(ui, &[track.id], true));
                    }
                    let center = egui::pos2(rect.left() + 56.0, rect.center().y);
                    style::record_fading(
                        ui.painter(),
                        center,
                        52.0,
                        art,
                        old_art.map(|id| (id, 1.0 - art_mix)),
                        label,
                        self.spin,
                    );
                    style::tonearm(
                        ui.painter(),
                        egui::pos2(rect.left() + 148.0, rect.top() + 2.0),
                        self.arm,
                        progress,
                        accent,
                        1.0,
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
                                    self.eq_button(ui);
                                    self.sleep_button(ui);
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
                                let shuffle = self.state.queue.shuffle;
                                if style::toggle_button(
                                    ui,
                                    style::Toggle::Shuffle,
                                    30.0,
                                    shuffle.then_some(accent),
                                )
                                .on_hover_text(if shuffle {
                                    "Shuffle: on"
                                } else {
                                    "Shuffle: off"
                                })
                                .clicked()
                                {
                                    self.state.queue.set_shuffle(!shuffle);
                                }
                                if style::icon_button(ui, Icon::Prev, 34.0, None, true).clicked() {
                                    self.action = Some(Action::Previous);
                                }
                                let icon = if self.state.effects.transitions() {
                                    Icon::Morph(ui.ctx().animate_bool_with_time(
                                        egui::Id::new("play-morph"),
                                        self.playing,
                                        0.18,
                                    ))
                                } else if self.playing {
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
                                let repeat = self.state.queue.repeat;
                                let (toggle, hint) = match repeat {
                                    Repeat::Off => (style::Toggle::Repeat, "Repeat: off"),
                                    Repeat::All => (style::Toggle::Repeat, "Repeat: all"),
                                    Repeat::One => (style::Toggle::RepeatOne, "Repeat: one"),
                                };
                                if style::toggle_button(
                                    ui,
                                    toggle,
                                    30.0,
                                    (repeat != Repeat::Off).then_some(accent),
                                )
                                .on_hover_text(hint)
                                .clicked()
                                {
                                    self.state.queue.repeat = match repeat {
                                        Repeat::Off => Repeat::All,
                                        Repeat::All => Repeat::One,
                                        Repeat::One => Repeat::Off,
                                    };
                                }
                                ui.add_space(10.0);
                                ui.label(time(self.state.position));
                                ui.add_space(4.0);
                                // reserve room for the duration label after the grooves
                                let width = (ui.available_width() - 56.0).max(60.0);
                                ui.allocate_ui(egui::vec2(width, 30.0), |ui| {
                                    let life = self.state.effects.seek_life().then_some(self.beat);
                                    let seek = style::groove_seek(
                                        ui,
                                        self.state.position,
                                        self.duration,
                                        self.reactive.peaks(),
                                        accent,
                                        life,
                                    );
                                    if let Some(position) = seek.to {
                                        self.action = Some(Action::Seek(position));
                                    }
                                    if seek.dragging {
                                        self.scrubbing = true;
                                        if self.state.effects.scratch() {
                                            // about one turn for every 90 points of drag
                                            self.spin += seek.drag_dx * 0.07;
                                        }
                                    }
                                });
                                ui.label(time(self.duration));
                            },
                        );
                    });
                });
            });
        if self.state.look.depth {
            // a light edge along the top of the bar, and a shadow cast upwards
            let top = panel.response.rect.top();
            let width = panel.response.rect.x_range();
            let p = ui.painter();
            for i in 0..4 {
                let rect = egui::Rect::from_x_y_ranges(
                    width,
                    top - 4.0 * (i + 1) as f32..=top - 4.0 * i as f32,
                );
                p.rect_filled(rect, 0.0, Color32::from_black_alpha(10));
            }
            p.hline(
                width,
                top + 0.5,
                egui::Stroke::new(1.0_f32, Color32::from_white_alpha(14)),
            );
        }
    }

    /// "Select" toggle plus the batch actions, shown above any track list. `ids` are the visible tracks.
    /// The "EQ" button in the player bar and the ten-band panel it opens.
    fn eq_button(&mut self, ui: &mut egui::Ui) {
        const PRESETS: [(&str, [f32; 10]); 5] = [
            ("Flat", [0.0; 10]),
            (
                "Bass boost",
                [6.0, 5.0, 4.0, 2.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            ),
            (
                "Treble boost",
                [0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 2.0, 4.0, 5.0, 6.0],
            ),
            (
                "Vocal",
                [-2.0, -2.0, -1.0, 1.0, 3.0, 3.0, 2.0, 0.0, -1.0, -2.0],
            ),
            ("Rock", [4.0, 3.0, 2.0, 0.0, -1.0, -1.0, 1.0, 3.0, 4.0, 4.0]),
        ];
        let mut eq = self.state.eq;
        let flat = eq.iter().all(|g| g.abs() < 0.05);
        let label = RichText::new("EQ").color(if flat { style::CREAM } else { self.accent });
        let config = egui::containers::menu::MenuConfig::new()
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside);
        egui::containers::menu::MenuButton::new(label)
            .config(config)
            .ui(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Preset");
                    for (name, gains) in PRESETS {
                        if ui.selectable_label(eq == gains, name).clicked() {
                            eq = gains;
                        }
                    }
                });
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().slider_width = 130.0;
                    ui.spacing_mut().item_spacing.x = 8.0;
                    for (i, hz) in EQ_BANDS.iter().enumerate() {
                        // a fixed-width column: `vertical_centered` would take the whole popup
                        ui.allocate_ui_with_layout(
                            egui::vec2(34.0, 190.0),
                            egui::Layout::top_down(egui::Align::Center),
                            |ui| {
                                ui.add(
                                    egui::Slider::new(&mut eq[i], -12.0..=12.0)
                                        .vertical()
                                        .show_value(false),
                                );
                                let name = if *hz >= 1000 {
                                    format!("{}k", hz / 1000)
                                } else {
                                    hz.to_string()
                                };
                                ui.label(RichText::new(name).small().weak());
                                ui.label(RichText::new(format!("{:+.0}", eq[i])).small());
                            },
                        );
                    }
                });
            });
        if eq != self.state.eq {
            self.action = Some(Action::Eq(eq));
        }
    }

    /// The playlist's tracks that are still in the library.
    fn playlist_ids(&self, p: usize) -> Vec<i64> {
        self.state.playlists.get(p).map_or_else(Vec::new, |list| {
            list.ids
                .iter()
                .copied()
                .filter(|id| self.by_id.contains_key(id))
                .collect()
        })
    }

    /// The selected tracks in the order they are listed.
    fn selected_ids(&self) -> Vec<i64> {
        if self.visible.is_empty() {
            self.tracks
                .iter()
                .map(|t| t.id)
                .filter(|id| self.selected.contains(id))
                .collect()
        } else {
            self.visible
                .iter()
                .copied()
                .filter(|id| self.selected.contains(id))
                .collect()
        }
    }

    fn add_to_playlist(&mut self, p: usize, ids: &[i64]) {
        if let Some(list) = self.state.playlists.get_mut(p) {
            for id in ids {
                if !list.ids.contains(id) {
                    list.ids.push(*id);
                }
            }
        }
    }

    /// "Add to playlist" submenu contents: each playlist, or a new one.
    fn playlist_menu(&mut self, ui: &mut egui::Ui, ids: &[i64]) {
        for i in 0..self.state.playlists.len() {
            let name = display(&self.state.playlists[i].name, "Untitled playlist").to_owned();
            if ui.button(name).clicked() {
                self.add_to_playlist(i, ids);
                ui.close();
            }
        }
        if ui.button("+ New playlist").clicked() {
            self.state.playlists.push(Playlist {
                name: "New playlist".into(),
                ids: Vec::new(),
            });
            let last = self.state.playlists.len() - 1;
            self.add_to_playlist(last, ids);
            ui.close();
        }
    }

    fn playlist_ui(&mut self, ui: &mut egui::Ui, p: usize) {
        if p >= self.state.playlists.len() {
            self.view = View::Tracks;
            self.rebuild();
            return;
        }
        ui.add_space(10.0);
        ui.add(
            egui::TextEdit::singleline(&mut self.state.playlists[p].name)
                .font(egui::TextStyle::Heading)
                .hint_text("Playlist name")
                .desired_width(360.0)
                .frame(false),
        );
        let ids = self.playlist_ids(p);
        ui.horizontal(|ui| {
            ui.label(format!("{} tracks", ids.len()));
            if ui
                .add_enabled(!ids.is_empty(), egui::Button::new("Play all"))
                .clicked()
            {
                self.action = Some(Action::StartPlaylist(0));
            }
            ui.menu_button("Delete playlist", |ui| {
                if ui.button("Yes, delete it").clicked() {
                    self.state.playlists.remove(p);
                    self.view = View::Tracks;
                    self.rebuild();
                    ui.close();
                }
            });
            if !self.selected.is_empty() && ui.button("Remove selected").clicked() {
                let gone = self.selected.clone();
                self.state.playlists[p].ids.retain(|id| !gone.contains(id));
                self.selected.clear();
            }
        });
        self.select_bar(ui, ids.clone());
        if ids.is_empty() {
            ui.add_space(20.0);
            ui.label("Empty. Right-click a track and choose Add to playlist, or select several tracks and use the Add to playlist button.");
            return;
        }
        let rows: Vec<usize> = ids
            .iter()
            .filter_map(|id| self.by_id.get(id).copied())
            .collect();
        style::scroll()
            .id_salt(("playlist", p))
            .show_rows(ui, 40.0, rows.len(), |ui, range| {
                for r in range {
                    self.track_row(ui, rows[r], None, r);
                }
            });
    }

    /// Asks for an image file; `poll_pick` applies it to the selected tracks.
    fn pick_cover_file(&mut self) {
        let (tx, rx) = mpsc::channel();
        self.pick_rx = Some(rx);
        let wake = self.wake.clone();
        std::thread::spawn(move || {
            let chosen = rfd::FileDialog::new()
                .set_title("Choose a cover image")
                .add_filter("Images", &["png", "jpg", "jpeg", "webp"])
                .pick_file();
            let _ = tx.send(chosen.map(|path| read_cover_file(&path)).transpose());
            wake();
        });
    }

    fn poll_pick(&mut self) {
        let Some(result) = self.pick_rx.as_ref().and_then(|rx| rx.try_recv().ok()) else {
            return;
        };
        self.pick_rx = None;
        match result {
            Ok(Some(bytes)) => {
                let ids = self
                    .tracks
                    .iter()
                    .map(|t| t.id)
                    .filter(|id| self.selected.contains(id))
                    .collect();
                self.command(Command::SetCover(ids, bytes, self.state.embed_cover));
                self.select_mode = false;
                self.selected.clear();
            }
            Ok(None) => {}
            Err(error) => self.message(format!("Could not use that cover: {error}")),
        }
    }

    /// What can be done with tracks, shared by the Select bar (`menu` false, as buttons) and the
    /// right-click menu (`menu` true), so both always offer the same things.
    fn track_actions(&mut self, ui: &mut egui::Ui, ids: &[i64], menu: bool) {
        let some = !ids.is_empty();
        // the dialogs below work on the selection, so make it exactly these tracks
        let select = |this: &mut Self| {
            this.selected = ids.iter().copied().collect();
            this.select_mode = true;
        };
        let mut done = false;
        if ids.len() == 1
            && ui
                .add_enabled(some, egui::Button::new("Edit tags / filename"))
                .clicked()
        {
            self.action = Some(Action::Edit(ids[0]));
            done = true;
        }
        if ui
            .add_enabled(some, egui::Button::new("Add to queue"))
            .clicked()
        {
            for id in ids {
                self.state.queue.enqueue(*id);
            }
            done = true;
        }
        ui.add_enabled_ui(some, |ui| {
            ui.menu_button("Add to playlist", |ui| self.playlist_menu(ui, ids));
            ui.menu_button("Rate", |ui| self.rate_menu(ui, ids));
        });
        let free = some && !self.job_running();
        if ui
            .add_enabled(
                free && self.state.lyrics_online,
                egui::Button::new("Find lyrics"),
            )
            .on_hover_text("Look up lyrics for every one of these songs in the background, using the services ticked in the settings. Turn on \"Look up lyrics online\" first.")
            .clicked()
        {
            self.lyrics_job(ids, false);
            done = true;
        }
        if ui
            .add_enabled(free, egui::Button::new("Save lyrics into files"))
            .on_hover_text("Write the lyrics Skratch already holds for these songs into the audio files' own tags, so other players show them too.")
            .clicked()
        {
            self.lyrics_job(ids, true);
            done = true;
        }
        if ui
            .add_enabled(
                some && self.pick_rx.is_none(),
                egui::Button::new("Set cover from image..."),
            )
            .on_hover_text("Use one image as the cover of every selected track")
            .clicked()
        {
            select(self);
            self.pick_cover_file();
            done = true;
        }
        if ui
            .add_enabled(some, egui::Button::new("Fix tags from filenames..."))
            .clicked()
        {
            select(self);
            self.fill_open = true;
            done = true;
        }
        if ui
            .add_enabled(
                some && self.fetch.is_none(),
                egui::Button::new("Fetch metadata..."),
            )
            .clicked()
        {
            select(self);
            self.open_fetch();
            done = true;
        }
        if ui
            .add_enabled(some, egui::Button::new("Find cover online..."))
            .clicked()
        {
            self.action = Some(Action::FindCover(ids.to_vec()));
            done = true;
        }
        if done && menu {
            ui.close();
        }
    }

    fn select_bar(&mut self, ui: &mut egui::Ui, ids: Vec<i64>) {
        self.visible = ids.clone();
        let typing = ui.ctx().wants_keyboard_input();
        if !typing && ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::A)) {
            self.select_mode = true;
            self.selected.extend(ids.iter().copied());
        }
        if !typing && self.select_mode && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.select_mode = false;
            self.selected.clear();
        }
        self.job_line(ui);
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
                let picked = self.selected_ids();
                self.track_actions(ui, &picked, false);
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
                style::scroll().max_height(320.0).show_rows(ui, 20.0, rows.len(), |ui, range| {
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
        let mut detail = format!(
            "{}  -  {}",
            display(&track.artist, "Unknown artist"),
            display(&track.album, "Unknown album")
        );
        if track.rating > 0 {
            detail.push_str(&format!("  ·  {}", listen::stars(track.rating)));
        }
        if track.plays > 0 {
            detail.push_str(&format!(
                "  ·  {} play{}",
                track.plays,
                if track.plays == 1 { "" } else { "s" }
            ));
        }
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
        // Ctrl-click toggles a track and Shift-click selects a range, with no need to switch
        // select mode on first.
        let modifiers = ui.input(|i| i.modifiers);
        let modified = modifiers.command || modifiers.shift;
        if response.clicked() && (picking || modified) {
            self.select_mode = true;
            let range = modifiers
                .shift
                .then_some(self.anchor)
                .flatten()
                .and_then(|anchor| {
                    let from = self.visible.iter().position(|v| *v == anchor)?;
                    let to = self.visible.iter().position(|v| *v == id)?;
                    Some(self.visible[from.min(to)..=from.max(to)].to_vec())
                });
            match range {
                Some(ids) => self.selected.extend(ids),
                None => {
                    if !self.selected.remove(&id) {
                        self.selected.insert(id);
                    }
                    self.anchor = Some(id);
                }
            }
        }
        let activated = if picking || modified {
            false
        } else if self.state.double_click {
            response.double_clicked()
        } else {
            response.clicked()
        };
        if activated {
            self.action = Some(self.start_action(id, queue_index, context_index));
        }
        response.context_menu(|ui| {
            if ui.button("Play").clicked() {
                self.action = Some(self.start_action(id, queue_index, context_index));
                ui.close();
            }
            // on a selected row the menu acts on the whole selection, like the Select bar
            let targets = if self.selected.contains(&id) {
                self.selected_ids()
            } else {
                vec![id]
            };
            self.track_actions(ui, &targets, true);
            if let View::Playlist(p) = self.view
                && ui.button("Remove from playlist").clicked()
            {
                if let Some(list) = self.state.playlists.get_mut(p) {
                    list.ids.retain(|v| !targets.contains(v));
                }
                ui.close();
            }
        });
    }

    /// How far the page just switched to has faded in (0 to 1). Opacity is applied when painting,
    /// so nothing is laid out again while it runs.
    fn page_fade(&mut self, ctx: &egui::Context) -> f32 {
        let key = (self.view, self.selected_group);
        let now = ctx.input(|i| i.time);
        if key != self.shown {
            self.shown = key;
            self.shown_at = now;
        }
        if !self.state.effects.transitions() {
            return 1.0;
        }
        if now - self.shown_at < 0.7 {
            ctx.request_repaint();
        }
        let t = ((now - self.shown_at) / 0.16).clamp(0.0, 1.0) as f32;
        1.0 - (1.0 - t).powi(3)
    }

    fn library_ui(&mut self, ui: &mut egui::Ui) {
        let fade = self.page_fade(ui.ctx());
        self.fade = fade;
        let wash = (self.state.look.mood).then_some(self.accent);
        let enter = |ui: &mut egui::Ui| {
            if let Some(accent) = wash {
                style::wash(ui, accent);
            }
            ui.set_opacity(fade);
            ui.add_space(6.0 * (1.0 - fade));
        };
        if self.view == View::Playing {
            self.playing_ui(ui);
            return;
        }
        if self.view == View::Downloads {
            egui::CentralPanel::default().show_inside(ui, |ui| {
                enter(ui);
                self.download_ui(ui)
            });
            return;
        }
        if self.view == View::Tidy {
            egui::CentralPanel::default().show_inside(ui, |ui| {
                enter(ui);
                self.tidy_ui(ui)
            });
            return;
        }
        if let View::Playlist(p) = self.view {
            egui::CentralPanel::default().show_inside(ui, |ui| {
                enter(ui);
                self.playlist_ui(ui, p)
            });
            return;
        }
        egui::CentralPanel::default().show_inside(ui, |ui| {
            enter(ui);
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                ui.heading(self.view.title());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let search = ui.add(egui::TextEdit::singleline(&mut self.search).id(SEARCH_ID.into()).hint_text("Search your music  (Ctrl+F)").desired_width(280.0));
                    if search.changed() { self.rebuild(); }
                    // egui drops focus on Escape before the widget is shown, so it is `lost_focus` here
                    if search.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                        self.search.clear();
                        self.rebuild();
                    }
                });
            });
            let old = (self.sort, self.descending);
            ui.horizontal(|ui| {
                egui::ComboBox::from_id_salt("sort").selected_text(self.sort.title()).show_ui(ui, |ui| {
                    for sort in [Sort::Album, Sort::Artist, Sort::Title, Sort::Year, Sort::Filename, Sort::Recent, Sort::Plays, Sort::Rating] { ui.selectable_value(&mut self.sort, sort, sort.title()); }
                });
                // newest, most played and best rated first is what these are for
                if old.0 != self.sort && matches!(self.sort, Sort::Recent | Sort::Plays | Sort::Rating) { self.descending = true; }
                let shown = self.state.show;
                egui::ComboBox::from_id_salt("show").selected_text(shown.title()).show_ui(ui, |ui| {
                    for show in Show::ALL { ui.selectable_value(&mut self.state.show, show, show.title()); }
                });
                if shown != self.state.show { self.rebuild(); }
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
                style::scroll().id_salt("queue").show_rows(ui, 40.0, indices.len(), |ui, range| {
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
                    style::scroll().id_salt("group-tracks").show_rows(ui, 40.0, count, |ui, range| {
                        for r in range { let i = self.groups[group_index].indices[r]; self.track_row(ui, i, None, r); }
                    });
                } else {
                    let missing: Vec<i64> = self.filtered.iter().map(|i| &self.tracks[*i]).filter(|t| t.artwork.is_none()).map(|t| t.id).collect();
                    if self.view == View::Albums && !missing.is_empty() {
                        let label = format!("Find covers for the {} tracks without one...", missing.len());
                        if ui.button(label).clicked() { self.action = Some(Action::FindCover(missing)); }
                    }
                    let columns = ((ui.available_width() / 180.0) as usize).max(1);
                    let since = ui.input(|i| i.time) - self.shown_at;
                    let stagger = self.state.effects.transitions();
                    let depth = self.state.look.depth;
                    let rows = self.groups.len().div_ceil(columns);
                    style::scroll().id_salt("group-grid").show_rows(ui, 230.0, rows, |ui, range| {
                        for row in range {
                            ui.horizontal(|ui| {
                                for col in 0..columns {
                                    let index = row * columns + col;
                                    let Some(group) = self.groups.get(index) else { break; };
                                    ui.allocate_ui_with_layout(egui::vec2(170.0, 222.0), egui::Layout::top_down(egui::Align::LEFT), |ui| {
                                        const SIZE: f32 = 145.0;
                                        ui.set_min_size(egui::vec2(170.0, 222.0));
                                        // the first cards rise in one after another
                                        if stagger && index < 12 {
                                            let a = ((since - 0.03 * index as f64) / 0.22).clamp(0.0, 1.0) as f32;
                                            ui.multiply_opacity(a);
                                            ui.add_space(8.0 * (1.0 - a * a * (3.0 - 2.0 * a)));
                                        }
                                        let seed = format!("{}{}", group.name, group.subtitle);
                                        let origin = ui.cursor().min;
                                        let sleeve = egui::Rect::from_min_size(origin, egui::vec2(SIZE, SIZE));
                                        if depth {
                                            style::shadow(ui.painter(), sleeve, 6.0);
                                        }
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
                style::scroll().id_salt("all-tracks").show_rows(ui, 40.0, self.filtered.len(), |ui, range| {
                    for r in range { let i = self.filtered[r]; self.track_row(ui, i, None, r); }
                });
            }
        });
    }

    fn download_ui(&mut self, ui: &mut egui::Ui) {
        ui.add_space(10.0);
        ui.heading("Downloads");
        ui.add_space(12.0);
        if !self.tools.downloads_ready() {
            ui.horizontal(|ui| {
                ui.colored_label(self.accent, "Downloading needs yt-dlp and ffmpeg.");
                if ui.button("Set up downloads...").clicked() {
                    self.open_setup(true);
                }
            });
            ui.add_space(8.0);
        }
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
                        && self.tools.downloads_ready()
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
            style::scroll()
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
                    cover.status = format!("Could not use that cover: {error}");
                }
                CoverMsg::Cancelled => {
                    cover.working = false;
                    cover.status.clear();
                }
            }
        }
        let embed = &mut self.state.embed_cover;
        let accent = self.accent;
        let (mut open, mut search, mut apply, mut cancel, mut skip, mut pick) =
            (true, false, false, false, false, false);
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
                    if ui.add_enabled(!cover.working, egui::Button::new("Choose image file...")).clicked() {
                        pick = true;
                    }
                });
                if !cover.status.is_empty() {
                    ui.label(RichText::new(&cover.status).color(if cover.working { style::CREAM_DIM } else { accent }));
                }
                style::scroll().max_height(410.0).show(ui, |ui| {
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
        } else if pick {
            let (tx, rx) = mpsc::channel();
            let wake = wake.clone();
            std::thread::spawn(move || {
                let message = match rfd::FileDialog::new()
                    .set_title("Choose a cover image")
                    .add_filter("Images", &["png", "jpg", "jpeg", "webp"])
                    .pick_file()
                {
                    Some(path) => CoverMsg::Full(read_cover_file(&path)),
                    None => CoverMsg::Cancelled,
                };
                let _ = tx.send(message);
                wake();
            });
            cover.rx = Some(rx);
            cover.working = true;
            cover.status = "Waiting for you to choose a file...".into();
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
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        style::release_after_grab(ctx, raw_input);
    }

    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        self.frame_at = Instant::now();
        self.poll(ctx);
        self.poll_setup();
        self.poll_pick();
        self.visible.clear();
        // keys are for this window only; whatever else has focus (a game, say) keeps its own
        let focused = ctx.input(|i| i.focused);
        if focused && ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::F)) {
            if self.view == View::Playing {
                // Up next and Library share one search box, so those two stay where they are
                if !matches!(self.state.now_panel, NowPanel::UpNext | NowPanel::Library) {
                    self.state.now_panel = NowPanel::Library;
                    self.listed = None;
                }
            } else if self.view == View::Downloads {
                self.view = View::Tracks;
                self.selected_group = None;
                self.rebuild();
            }
            ctx.memory_mut(|m| m.request_focus(egui::Id::new(SEARCH_ID)));
        }
        if focused
            && !ctx.wants_keyboard_input()
            && ctx.input(|input| input.key_pressed(egui::Key::Space))
        {
            self.perform(Action::Pause);
        }
        // arrow keys scrub, unless a text box is using them
        if focused && !ctx.wants_keyboard_input() && self.state.queue.current_id().is_some() {
            for (key, seconds) in [(egui::Key::ArrowLeft, -5.0), (egui::Key::ArrowRight, 5.0)] {
                // one call takes one key event, and a held key can bring several per frame
                while ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, key)) {
                    self.perform(Action::Seek(self.state.position + seconds));
                }
            }
        }
        self.animate(ctx);
        let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
        let radius = if maximized { 0 } else { style::RADIUS };
        let root = Frame::new()
            .fill(self.mood(style::BG, 0.04))
            .corner_radius(CornerRadius::same(radius));
        egui::CentralPanel::default().frame(root).show(ctx, |ui| {
            if self.viz.full {
                self.viz_screen(ui);
                return;
            }
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
            if self.state.look.grain {
                // inside the rounded corners, which are see-through
                let inset = if maximized { 0.0 } else { 4.0 };
                style::grain(ui.painter(), outline.shrink(inset), &self.grain);
            }
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
        self.fetch_window(ctx);
        self.setup_window(ctx);
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

/// Reads a cover image the user picked, refusing anything unreasonably large.
fn read_cover_file(path: &std::path::Path) -> Result<Vec<u8>, String> {
    const MAX: u64 = 50 * 1024 * 1024;
    let size = std::fs::metadata(path).map_err(|e| e.to_string())?.len();
    if size > MAX {
        return Err("that file is over 50 MB".into());
    }
    std::fs::read(path).map_err(|e| e.to_string())
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
pub(crate) fn time(seconds: f64) -> String {
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
