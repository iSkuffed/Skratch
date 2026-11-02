//! Listening: what has been played and rated, the sleep timer, fades and gapless playback, the
//! background lyrics jobs, the playback settings and the "Tidy up" page.
use super::{Action, Skratch, View, display, tidyrow, time};
use crate::{
    service::Command,
    state::Show,
    style::{self},
};
use eframe::egui::{self, RichText, vec2};
use skratch::{
    health::{self, Problem},
    lyrics::{self, Embedded},
    model::{MetadataEdit, Track},
    player::Loudness,
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

/// How long the sound takes to fade out before the sleep timer stops it, in seconds.
const SLEEP_FADE: f64 = 20.0;

/// How far from the end of a song the next one is lined up for gapless playback, in seconds.
const PRELOAD_AHEAD: f64 = 20.0;

/// Seconds a song fades for in the "Fade between songs" menu.
const FADES: [u8; 5] = [0, 2, 4, 6, 10];

#[derive(Clone, Copy, PartialEq)]
pub enum Sleep {
    /// Stop playing at this time.
    At(Instant),
    /// Stop when the song that is playing ends.
    EndOfSong,
}

/// What the Tidy up page lists, worked out once per library change.
pub struct Tidy {
    /// Songs that appear more than once, as headers and the tracks under them.
    copies: Vec<CopyRow>,
    groups: usize,
    /// Tracks with unfinished tags and what is wrong with each.
    loose: Vec<(usize, Vec<Problem>)>,
}

enum CopyRow {
    Header(String),
    Track(usize, Vec<i64>),
}

/// A lyrics lookup or save running over many songs in the background.
struct Job {
    rx: mpsc::Receiver<JobEvent>,
    cancel: Arc<AtomicBool>,
    what: &'static str,
    done: usize,
    total: usize,
    found: usize,
}

enum JobEvent {
    Progress { found: bool },
    Finished(String),
}

/// Everything about listening that is not saved: it starts afresh with each launch.
pub struct Listen {
    pub sleep: Option<Sleep>,
    /// The fade last given to mpv, so it is only told when it changes.
    pub gain: f64,
    /// Whether mpv was told to loop the song (while lyrics are being edited).
    looping: bool,
    /// Seconds of the playing song actually heard, for counting a play.
    listened: f64,
    counted: bool,
    /// The song lined up in mpv behind the one playing.
    pub preloaded: Option<i64>,
    /// Where that song was when it was lined up; a rename needs it lined up again.
    preloaded_path: Option<std::path::PathBuf>,
    /// When the position last came in, to estimate where the song is between updates.
    pub pos_at: Instant,
    tick_at: Instant,
    job: Option<Job>,
    pub tidy: Option<Tidy>,
    tidy_tab: TidyTab,
    tidy_only: Option<Problem>,
}

#[derive(Clone, Copy, PartialEq)]
enum TidyTab {
    Copies,
    Tags,
    Suggestions,
    Excluded,
}

impl Default for Listen {
    fn default() -> Self {
        Self {
            sleep: None,
            gain: 1.0,
            looping: false,
            listened: 0.0,
            counted: false,
            preloaded: None,
            preloaded_path: None,
            pos_at: Instant::now(),
            tick_at: Instant::now(),
            job: None,
            tidy: None,
            tidy_tab: TidyTab::Copies,
            tidy_only: None,
        }
    }
}

impl Listen {
    /// A new song has started playing.
    pub fn restart(&mut self) {
        self.listened = 0.0;
        self.counted = false;
        self.preloaded = None;
        self.preloaded_path = None;
        self.pos_at = Instant::now();
    }

    /// A fresh mpv has no fade applied.
    pub fn new_player(&mut self) {
        self.gain = 1.0;
        self.looping = false;
    }
}

/// The fade at `position` in a song `duration` long that lasts `seconds` at each end.
pub fn fade_gain(seconds: u8, position: f64, duration: f64) -> f64 {
    let fade = f64::from(seconds);
    if fade <= 0.0 || duration <= 4.0 * fade {
        return 1.0;
    }
    (position / fade)
        .min((duration - position) / fade)
        .clamp(0.0, 1.0)
}

pub(super) fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// "★★★" for a rating, empty when unrated.
pub fn stars(rating: u8) -> String {
    "★".repeat(usize::from(rating.min(5)))
}

impl Skratch {
    /// Runs every frame while a song is loaded: counts a play, applies fades and the sleep
    /// timer, and lines the next song up for gapless playback.
    pub(super) fn playback_tick(&mut self, ctx: &egui::Context) {
        let now = Instant::now();
        let dt = now
            .duration_since(self.listen.tick_at)
            .as_secs_f64()
            .min(1.0);
        self.listen.tick_at = now;
        if !self.loaded || self.player.is_none() {
            return;
        }
        let duration = self.duration;
        if self.playing {
            self.listen.listened += dt;
        }
        let at = if self.playing {
            self.state.position + self.listen.pos_at.elapsed().as_secs_f64()
        } else {
            self.state.position
        };
        let at = if duration > 0.0 { at.min(duration) } else { at };

        if !self.listen.counted
            && duration > 0.0
            && self.listen.listened >= (duration * 0.5).min(240.0)
        {
            self.listen.counted = true;
            self.played_through();
        }

        let mut gain = fade_gain(self.state.fade_secs, at, duration);
        match self.listen.sleep {
            Some(Sleep::At(when)) => {
                let left = when.saturating_duration_since(now).as_secs_f64();
                if left <= 0.0 {
                    self.listen.sleep = None;
                    if self.playing {
                        self.perform(Action::Pause);
                    }
                } else {
                    gain *= (left / SLEEP_FADE).clamp(0.0, 1.0);
                    ctx.request_repaint_after(Duration::from_millis(if left < SLEEP_FADE {
                        50
                    } else {
                        1000
                    }));
                }
            }
            Some(Sleep::EndOfSong) if duration > 0.0 => {
                gain *= ((duration - at) / 8.0).clamp(0.0, 1.0);
            }
            _ => {}
        }
        if ((gain - self.listen.gain).abs() >= 0.01
            || (gain >= 0.999) != (self.listen.gain >= 0.999))
            && let Some(player) = self.player.as_mut()
            && player.set_fade(gain).is_ok()
        {
            self.listen.gain = gain;
        }
        if gain < 0.999 && self.playing {
            ctx.request_repaint_after(Duration::from_millis(50));
        }

        // while lyrics are being edited the song goes round again, so the editor is not left
        let editing = self.lyrics.editing.is_some();
        if editing != self.listen.looping
            && let Some(player) = self.player.as_mut()
            && player.set_loop(editing).is_ok()
        {
            self.listen.looping = editing;
        }
        let want = (!self.state.gapless_off
            && !editing
            && duration > 0.0
            && duration - at <= PRELOAD_AHEAD
            && self.listen.sleep != Some(Sleep::EndOfSong))
        .then(|| self.state.queue.peek_next())
        .flatten();
        let path = want.and_then(|id| Some(self.tracks[*self.by_id.get(&id)?].path.clone()));
        if (want != self.listen.preloaded || path != self.listen.preloaded_path)
            && let Some(player) = self.player.as_mut()
        {
            if self.listen.preloaded.is_some() {
                let _ = player.clear_preload();
            }
            self.listen.preloaded = None;
            self.listen.preloaded_path = None;
            if let Some(path) = path
                && player.preload(&path).is_ok()
            {
                self.listen.preloaded = want;
                self.listen.preloaded_path = Some(path);
            }
        }
    }

    /// The song has been listened to enough to count: bump its play count and, when asked to,
    /// save the lyrics Skratch holds for it into the file.
    fn played_through(&mut self) {
        let Some(track) = self.current().cloned() else {
            return;
        };
        let when = now_secs();
        if let Some(&i) = self.by_id.get(&track.id) {
            self.tracks[i].plays += 1;
            self.tracks[i].last_played = Some(when);
        }
        self.command(Command::Played(track.id, when));
        if self.state.show != Show::All
            || matches!(self.sort, super::Sort::Recent | super::Sort::Plays)
        {
            self.rebuild();
        }
        if self.state.lyrics_embed {
            let dir = self.data_dir.join("lyrics");
            std::thread::spawn(move || {
                let _ = lyrics::embed(&track, &dir);
            });
        }
    }

    /// The song mpv moved on to by itself is now the one playing.
    pub(super) fn adopt_preloaded(&mut self) {
        self.listen.restart();
        self.pending_seek = None;
        self.state.position = 0.0;
        if let Some(track) = self.current() {
            self.duration = track.duration;
        }
    }

    pub(super) fn rate(&mut self, ids: &[i64], stars: u8) {
        for id in ids {
            if let Some(&i) = self.by_id.get(id) {
                self.tracks[i].rating = stars;
            }
        }
        self.command(Command::Rate(ids.to_vec(), stars));
        if self.state.show == Show::Rated {
            self.rebuild();
        }
    }

    /// Five stars that rate `track` when clicked; clicking the rating it has clears it.
    pub(super) fn star_row(&mut self, ui: &mut egui::Ui, track: &Track) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            for n in 1..=5_u8 {
                let on = n <= track.rating;
                let star = RichText::new("★").size(22.0).color(if on {
                    self.accent
                } else {
                    style::CREAM_DIM.gamma_multiply(0.45)
                });
                let response = ui
                    .add(egui::Label::new(star).sense(egui::Sense::click()))
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .on_hover_text(if n == track.rating {
                        "Clear the rating".to_owned()
                    } else {
                        format!("{n} star{}", if n == 1 { "" } else { "s" })
                    });
                if response.clicked() {
                    self.rate(&[track.id], if n == track.rating { 0 } else { n });
                }
            }
            if track.plays > 0 {
                ui.add_space(10.0);
                ui.label(
                    RichText::new(format!(
                        "{} play{}",
                        track.plays,
                        if track.plays == 1 { "" } else { "s" }
                    ))
                    .color(style::CREAM_DIM),
                );
            }
        });
    }

    /// The "Rate" submenu for a track list's menus.
    pub(super) fn rate_menu(&mut self, ui: &mut egui::Ui, ids: &[i64]) {
        for n in (0..=5_u8).rev() {
            let label = if n == 0 {
                "Clear rating".to_owned()
            } else {
                stars(n)
            };
            if ui.button(label).clicked() {
                self.rate(ids, n);
                ui.close();
            }
        }
    }

    /// The Sleep button in the player bar: a menu of timers, showing the time left when one runs.
    pub(super) fn sleep_button(&mut self, ui: &mut egui::Ui) {
        let label = match self.listen.sleep {
            Some(Sleep::At(when)) => {
                let left = when.saturating_duration_since(Instant::now()).as_secs();
                RichText::new(format!("Sleep {}", time(left as f64))).color(self.accent)
            }
            Some(Sleep::EndOfSong) => RichText::new("Sleep: song").color(self.accent),
            None => RichText::new("Sleep").color(style::CREAM),
        };
        egui::containers::menu::MenuButton::new(label).ui(ui, |ui| {
            if ui
                .selectable_label(self.listen.sleep.is_none(), "Off")
                .clicked()
            {
                self.listen.sleep = None;
                ui.close();
            }
            for minutes in [15_u64, 30, 45, 60, 90] {
                if ui.button(format!("In {minutes} minutes")).clicked() {
                    self.listen.sleep = Some(Sleep::At(
                        Instant::now() + Duration::from_secs(minutes * 60),
                    ));
                    ui.close();
                }
            }
            if ui
                .selectable_label(
                    self.listen.sleep == Some(Sleep::EndOfSong),
                    "At the end of this song",
                )
                .clicked()
            {
                self.listen.sleep = Some(Sleep::EndOfSong);
                ui.close();
            }
            ui.label(
                RichText::new("The sound fades out, then playback pauses.")
                    .small()
                    .weak(),
            );
        });
    }

    /// Sidebar settings for how songs run into each other and how loud they are.
    pub(super) fn playback_settings(&mut self, ui: &mut egui::Ui) {
        let mut gapless = !self.state.gapless_off;
        ui.checkbox(&mut gapless, "Gapless playback")
            .on_hover_text("The next song is lined up ahead of time so albums play without a pause between tracks.");
        self.state.gapless_off = !gapless;
        ui.horizontal(|ui| {
            ui.label("Fade between songs");
            egui::ComboBox::from_id_salt("fade-secs")
                .width(56.0)
                .selected_text(fade_label(self.state.fade_secs))
                .show_ui(ui, |ui| {
                    for seconds in FADES {
                        ui.selectable_value(&mut self.state.fade_secs, seconds, fade_label(seconds));
                    }
                });
        })
        .response
        .on_hover_text("Each song fades out at its end and the next fades in. It dips and returns rather than overlapping two songs.");
        let before = self.state.loudness;
        ui.horizontal(|ui| {
            ui.label("Even out loudness");
            egui::ComboBox::from_id_salt("loudness")
                .width(72.0)
                .selected_text(self.state.loudness.label())
                .show_ui(ui, |ui| {
                    for mode in Loudness::ALL {
                        ui.selectable_value(&mut self.state.loudness, mode, mode.label())
                            .on_hover_text(mode.hint());
                    }
                });
        })
        .response
        .on_hover_text(self.state.loudness.hint());
        if before != self.state.loudness
            && let Some(player) = self.player.as_mut()
            && let Err(error) = player.set_loudness(self.state.loudness)
        {
            self.message(format!("Cannot change loudness: {error:#}"));
        }
    }

    // ---- lyrics for many songs --------------------------------------------------------

    /// Looks lyrics up for these songs one after another in the background (and saves them into
    /// the files too when that is switched on), or only saves what Skratch already holds.
    pub(super) fn lyrics_job(&mut self, ids: &[i64], save_only: bool) {
        if self.listen.job.is_some() {
            return;
        }
        let tracks: Vec<Track> = ids
            .iter()
            .filter_map(|id| Some(self.tracks[*self.by_id.get(id)?].clone()))
            .collect();
        if tracks.is_empty() {
            return;
        }
        let (dir, online, embed) = (
            self.data_dir.join("lyrics"),
            self.state.lyric_sources(),
            save_only || self.state.lyrics_embed,
        );
        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let stop = cancel.clone();
        let wake = self.wake.clone();
        let total = tracks.len();
        std::thread::spawn(move || {
            let (mut found, mut saved, mut had, mut failed) = (0, 0, 0, 0);
            for track in &tracks {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                let has = if save_only {
                    true
                } else {
                    lyrics::fetch(track, &dir, online).is_some()
                };
                if has && embed {
                    match lyrics::embed(track, &dir) {
                        Ok(Embedded::Written) => saved += 1,
                        Ok(Embedded::AlreadyThere) => had += 1,
                        Ok(Embedded::NoLyrics) => {}
                        Err(_) => failed += 1,
                    }
                }
                found += usize::from(has);
                let _ = tx.send(JobEvent::Progress { found: has });
                wake();
                // be polite to the services
                if !save_only && online.allowed {
                    std::thread::sleep(Duration::from_millis(250));
                }
            }
            let cut = stop.load(Ordering::Relaxed);
            let mut message = if save_only {
                format!("Saved lyrics into {saved} of {total} songs")
            } else {
                format!("Found lyrics for {found} of {total} songs")
            };
            if !save_only && embed {
                message.push_str(&format!(", saved into {saved} files"));
            }
            if save_only && had > 0 {
                message.push_str(&format!(" ({had} already had them)"));
            }
            if failed > 0 {
                message.push_str(&format!(", {failed} files could not be written"));
            }
            if cut {
                message.push_str(" before it was stopped");
            }
            let _ = tx.send(JobEvent::Finished(format!("{message}.")));
            wake();
        });
        self.listen.job = Some(Job {
            rx,
            cancel,
            what: if save_only {
                "Saving lyrics into files"
            } else {
                "Looking up lyrics"
            },
            done: 0,
            total,
            found: 0,
        });
    }

    /// Takes in the job's progress, and shows it with a Stop button when it is running.
    pub(super) fn job_line(&mut self, ui: &mut egui::Ui) {
        let mut finished = None;
        if let Some(job) = &mut self.listen.job {
            while let Ok(event) = job.rx.try_recv() {
                match event {
                    JobEvent::Progress { found } => {
                        job.done += 1;
                        job.found += usize::from(found);
                    }
                    JobEvent::Finished(message) => finished = Some(message),
                }
            }
        }
        if let Some(message) = finished {
            self.listen.job = None;
            self.message(message);
        }
        if let Some(job) = &self.listen.job {
            ui.ctx().request_repaint_after(Duration::from_millis(300));
            let (text, fraction) = (
                format!(
                    "{}: {} of {} done, {} found",
                    job.what, job.done, job.total, job.found
                ),
                job.done as f32 / job.total.max(1) as f32,
            );
            let cancel = job.cancel.clone();
            ui.horizontal(|ui| {
                ui.add(egui::ProgressBar::new(fraction).desired_width(160.0));
                ui.label(text);
                if ui.small_button("Stop").clicked() {
                    cancel.store(true, Ordering::Relaxed);
                }
            });
        }
    }

    /// Whether a lyrics job is running (only one at a time).
    pub(super) fn job_running(&self) -> bool {
        self.listen.job.is_some()
    }

    // ---- Tidy up ----------------------------------------------------------------------

    fn tidy_data(&self) -> Tidy {
        let groups = health::duplicates(&self.tracks);
        let mut copies = Vec::new();
        for group in &groups {
            let first = &self.tracks[group[0]];
            copies.push(CopyRow::Header(format!(
                "{} - {}  ({} copies)",
                display(&first.artist, "Unknown artist"),
                first.title,
                group.len()
            )));
            copies.extend(group.iter().map(|i| {
                // the other copies of this song, for "keep only this one"
                let others = group
                    .iter()
                    .filter(|j| *j != i)
                    .map(|j| self.tracks[*j].id)
                    .collect();
                CopyRow::Track(*i, others)
            }));
        }
        Tidy {
            copies,
            groups: groups.len(),
            loose: health::untidy(&self.tracks, &[]),
        }
    }

    pub(super) fn tidy_ui(&mut self, ui: &mut egui::Ui) {
        ui.add_space(10.0);
        ui.heading(View::Tidy.title());
        ui.add_space(8.0);
        if self.listen.tidy.is_none() {
            self.listen.tidy = Some(self.tidy_data());
        }
        let tidy = self.listen.tidy.take().expect("tidy data was just made");
        ui.horizontal(|ui| {
            ui.selectable_value(
                &mut self.listen.tidy_tab,
                TidyTab::Copies,
                format!("Duplicates ({})", tidy.groups),
            );
            ui.selectable_value(
                &mut self.listen.tidy_tab,
                TidyTab::Tags,
                format!("Unfinished tags ({})", tidy.loose.len()),
            );
            ui.selectable_value(
                &mut self.listen.tidy_tab,
                TidyTab::Suggestions,
                format!("Suggestions ({})", self.suggest.waiting()),
            );
            ui.selectable_value(
                &mut self.listen.tidy_tab,
                TidyTab::Excluded,
                format!("Excluded ({})", self.excluded.len()),
            );
        });
        let hint = match self.listen.tidy_tab {
            TidyTab::Copies => {
                "The same song twice. Exclude hides a file from Skratch and keeps it on the disk; Delete removes it for good."
            }
            TidyTab::Tags => {
                "Songs whose tags look unfinished. Edit one, or use the buttons for all of them at once."
            }
            TidyTab::Suggestions => {
                "Found online for songs that lack an album, year, track number or cover. Nothing is written until you press Apply."
            }
            TidyTab::Excluded => "Songs Skratch skips. Their files are still on the disk.",
        };
        ui.add_space(4.0);
        ui.label(RichText::new(hint).size(12.0).color(style::CREAM_DIM));
        ui.add_space(4.0);
        match self.listen.tidy_tab {
            TidyTab::Copies => self.copies_page(ui, &tidy),
            TidyTab::Tags => self.loose_page(ui, &tidy),
            TidyTab::Suggestions => self.suggestions_page(ui),
            TidyTab::Excluded => self.excluded_page(ui),
        }
        // the page may have changed the library; the next snapshot rebuilds this
        if self.listen.tidy.is_none() {
            self.listen.tidy = Some(tidy);
        }
    }

    fn copies_page(&mut self, ui: &mut egui::Ui, tidy: &Tidy) {
        if tidy.copies.is_empty() {
            ui.add_space(20.0);
            ui.label("No song appears twice. Two files count as the same song when their title and artist match once filler like \"(Official Video)\" is ignored, and they are within a few seconds of the same length.");
            return;
        }
        let heights: Vec<f32> = tidy
            .copies
            .iter()
            .map(|row| match row {
                CopyRow::Header(_) => tidyrow::HEAD,
                CopyRow::Track(..) => tidyrow::ROW,
            })
            .collect();
        let total: f32 = heights.iter().sum();
        style::scroll()
            .id_salt("tidy-copies")
            .show_viewport(ui, |ui, view| {
                ui.set_height(total);
                let (left, width) = (ui.min_rect().min, ui.available_width());
                let mut top = 0.0;
                for (row, height) in tidy.copies.iter().zip(&heights) {
                    // only the rows in view are drawn; the rest are skipped by their height
                    if top + height >= view.min.y && top <= view.max.y {
                        let rect =
                            egui::Rect::from_min_size(left + vec2(0.0, top), vec2(width, *height));
                        ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| match row {
                            CopyRow::Header(text) => {
                                ui.add_space(10.0);
                                ui.label(
                                    RichText::new(text).strong().size(14.0).color(style::CREAM),
                                );
                            }
                            CopyRow::Track(i, others) => self.tidy_track(ui, *i, None, others),
                        });
                    }
                    top += height;
                }
            });
    }

    fn loose_page(&mut self, ui: &mut egui::Ui, tidy: &Tidy) {
        ui.horizontal_wrapped(|ui| {
            ui.label("Show");
            if ui
                .selectable_label(self.listen.tidy_only.is_none(), "Everything")
                .clicked()
            {
                self.listen.tidy_only = None;
            }
            for problem in Problem::ALL {
                let count = tidy
                    .loose
                    .iter()
                    .filter(|(_, found)| found.contains(&problem))
                    .count();
                if count > 0
                    && ui
                        .selectable_label(
                            self.listen.tidy_only == Some(problem),
                            format!("{} ({count})", problem.label()),
                        )
                        .clicked()
                {
                    self.listen.tidy_only = Some(problem);
                }
            }
        });
        let only = self.listen.tidy_only;
        let rows: Vec<&(usize, Vec<Problem>)> = tidy
            .loose
            .iter()
            .filter(|(_, found)| only.is_none_or(|p| found.contains(&p)))
            .collect();
        if rows.is_empty() {
            ui.add_space(20.0);
            ui.label("Every song's tags look finished.");
            return;
        }
        let ids: Vec<i64> = rows.iter().map(|(i, _)| self.tracks[*i].id).collect();
        self.visible = ids.clone();
        let picking = self.select_mode;
        if picking {
            // only what is listed counts, whatever was picked on another tab or filter
            self.selected.retain(|id| ids.contains(id));
        }
        let working: Vec<i64> = if picking {
            self.selected_ids()
        } else {
            ids.clone()
        };
        let albumless = |this: &Self, which: &[i64]| -> Vec<i64> {
            this.tracks
                .iter()
                .filter(|t| which.contains(&t.id) && health::problems(t).contains(&Problem::NoAlbum))
                .map(|t| t.id)
                .collect()
        };
        ui.horizontal_wrapped(|ui| {
            if ui.selectable_label(self.select_mode, "Select").clicked() {
                self.select_mode = !self.select_mode;
                self.selected.clear();
            }
            if picking {
                if ui.button("Select all").clicked() {
                    self.selected.extend(ids.iter().copied());
                }
                if ui.button("Clear").clicked() {
                    self.selected.clear();
                }
                ui.label(format!("{} selected", working.len()));
            } else {
                ui.label(format!("For all {} listed:", ids.len()));
            }
            self.actions_button(ui, &working);
            let single = albumless(self, &working);
            if ui
                .add_enabled(
                    !single.is_empty(),
                    egui::Button::new(format!("Mark {} as Singles", single.len())),
                )
                .on_hover_text("For songs that are not on an album: write \"Singles\" as the album. Songs that already have an album are left alone, and a single needs no track number.")
                .clicked()
            {
                self.mark_singles(&single);
                self.selected.clear();
            }
        });
        ui.separator();
        style::scroll().id_salt("tidy-loose").show_rows(
            ui,
            tidyrow::ROW,
            rows.len(),
            |ui, range| {
                for (i, found) in rows[range].iter().copied() {
                    let note = found
                        .iter()
                        .map(|p| p.label())
                        .collect::<Vec<_>>()
                        .join(", ");
                    self.tidy_track(ui, *i, Some(note), &[]);
                }
            },
        );
    }

    /// Writes "Singles" as the album of these songs.
    fn mark_singles(&mut self, ids: &[i64]) {
        let edits: Vec<(i64, MetadataEdit)> = self
            .tracks
            .iter()
            .filter(|t| ids.contains(&t.id))
            .map(|t| {
                let mut edit = MetadataEdit::from(t);
                edit.album = health::SINGLES.to_owned();
                (t.id, edit)
            })
            .collect();
        if !edits.is_empty() {
            self.command(Command::EditMany(edits));
        }
    }

    /// Ticks or unticks one listed song; Shift extends from the last one touched.
    fn pick(&mut self, id: i64, on: bool, range: bool) {
        let span = range
            .then_some(self.anchor)
            .flatten()
            .and_then(|anchor| {
                let from = self.visible.iter().position(|v| *v == anchor)?;
                let to = self.visible.iter().position(|v| *v == id)?;
                Some(self.visible[from.min(to)..=from.max(to)].to_vec())
            });
        match span {
            Some(ids) if on => self.selected.extend(ids),
            _ if on => {
                self.selected.insert(id);
            }
            _ => {
                self.selected.remove(&id);
            }
        }
        self.anchor = Some(id);
    }

    fn excluded_page(&mut self, ui: &mut egui::Ui) {
        if self.excluded.is_empty() {
            ui.add_space(20.0);
            ui.label("Nothing is excluded. Songs you exclude on the Duplicates or Unfinished tags tab are listed here, so they can be brought back.");
            return;
        }
        let paths = self.excluded.clone();
        if ui
            .button("Bring all back")
            .on_hover_text("Let scans find these files again")
            .clicked()
        {
            self.command(Command::Include(paths.clone()));
        }
        ui.separator();
        let height = 28.0;
        style::scroll()
            .id_salt("tidy-excluded")
            .show_rows(ui, height, paths.len(), |ui, range| {
                for path in &paths[range] {
                    ui.allocate_ui_with_layout(
                        vec2(ui.available_width(), height - 4.0),
                        egui::Layout::left_to_right(egui::Align::Center),
                        |ui| {
                            if ui.small_button("Bring back").clicked() {
                                self.command(Command::Include(vec![path.clone()]));
                            }
                            ui.add(
                                egui::Label::new(
                                    RichText::new(path.display().to_string())
                                        .color(style::CREAM_DIM),
                                )
                                .truncate(),
                            );
                        },
                    );
                }
            });
    }

    /// One track on the Tidy page, as a card: its cover and who it is, then what is wrong with
    /// it (or where the copy lives), and buttons to play, edit, exclude or delete it. `others`
    /// are the other copies of the same song (duplicates page), which "keep only this one"
    /// takes out.
    fn tidy_track(
        &mut self,
        ui: &mut egui::Ui,
        index: usize,
        note: Option<String>,
        others: &[i64],
    ) {
        let track = self.tracks[index].clone();
        let id = track.id;
        let picture = self.cover_of(&track);
        let accent = self.accent;
        let place = {
            let kind = track
                .path
                .extension()
                .map(|e| e.to_string_lossy().to_uppercase())
                .unwrap_or_default();
            let folder = track.path.parent().map(|p| p.display().to_string());
            format!("{kind}  ·  {}", folder.unwrap_or_default())
        };
        let picking = self.select_mode && self.listen.tidy_tab == TidyTab::Tags;
        let mut ticked = self.selected.contains(&id);
        let mut toggled = None;
        let no_album = note.is_some() && health::problems(&track).contains(&Problem::NoAlbum);
        tidyrow::card(
            ui,
            tidyrow::ROW,
            236.0,
            |ui| {
                ui.horizontal_top(|ui| {
                    if picking {
                        ui.vertical(|ui| {
                            ui.add_space(22.0);
                            if ui.checkbox(&mut ticked, "").clicked() {
                                toggled = Some((ticked, ui.input(|i| i.modifiers.shift)));
                            }
                        });
                    }
                    tidyrow::cover(ui, picture, "No cover");
                    ui.vertical(|ui| {
                        tidyrow::identity(ui, &track);
                        match &note {
                            Some(note) => ui.add(
                                egui::Label::new(
                                    RichText::new(format!("Needs: {note}"))
                                        .size(12.0)
                                        .color(accent),
                                )
                                .truncate(),
                            ),
                            None => ui.add(
                                egui::Label::new(
                                    RichText::new(place).size(12.0).color(style::CREAM_DIM),
                                )
                                .truncate(),
                            ),
                        };
                    });
                });
            },
            |ui| {
                ui.horizontal(|ui| {
                    if ui.small_button("Play").clicked() {
                        self.action = Some(Action::StartLibrary(id));
                    }
                    if ui.small_button("Edit").clicked() {
                        self.action = Some(Action::Edit(id));
                    }
                    if ui
                        .small_button("Exclude")
                        .on_hover_text("Take it out of Skratch. The file stays on the disk and scans skip it; Excluded lists it to bring back.")
                        .clicked()
                    {
                        self.command(Command::Exclude(vec![id]));
                    }
                    if confirm_button(
                        ui,
                        "Delete",
                        "This will delete the file from the disk. This CAN NOT be undone. Are you sure?",
                        "Yes, delete",
                    ) {
                        self.command(Command::Delete(vec![id]));
                    }
                });
                if no_album
                    && ui
                        .small_button("Single")
                        .on_hover_text("Not on an album: write \"Singles\" as its album")
                        .clicked()
                {
                    self.mark_singles(&[id]);
                }
                if !others.is_empty() {
                    ui.horizontal(|ui| {
                        if ui
                            .small_button("Keep only this")
                            .on_hover_text("Exclude the other copies of this song. Their files stay on the disk; Excluded brings them back.")
                            .clicked()
                        {
                            self.command(Command::Exclude(others.to_vec()));
                        }
                        if confirm_button(
                            ui,
                            "Delete others",
                            "This will delete the other copies from the disk and keep this one. This CAN NOT be undone. Are you sure?",
                            "Yes, delete them",
                        ) {
                            self.command(Command::Delete(others.to_vec()));
                        }
                    });
                }
            },
        );
        if let Some((on, range)) = toggled {
            self.pick(id, on, range);
        }
    }
}

/// A button that asks "are you sure?" in a small popup beside it; true when answered yes.
pub(super) fn confirm_button(ui: &mut egui::Ui, label: &str, question: &str, yes: &str) -> bool {
    let button = ui.small_button(label);
    let mut confirmed = false;
    egui::Popup::from_toggle_button_response(&button)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .show(|ui| {
            ui.set_max_width(280.0);
            ui.label(question);
            ui.horizontal(|ui| {
                if ui
                    .button(RichText::new(yes).color(egui::Color32::from_rgb(235, 100, 90)))
                    .clicked()
                {
                    confirmed = true;
                    ui.close();
                }
                if ui.button("No").clicked() {
                    ui.close();
                }
            });
        });
    confirmed
}

fn fade_label(seconds: u8) -> String {
    if seconds == 0 {
        "Off".into()
    } else {
        format!("{seconds} s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn songs_fade_in_and_out_only_when_long_enough() {
        assert_eq!(fade_gain(0, 0.0, 200.0), 1.0);
        assert_eq!(fade_gain(4, 100.0, 200.0), 1.0);
        assert_eq!(fade_gain(4, 0.0, 200.0), 0.0);
        assert!((fade_gain(4, 2.0, 200.0) - 0.5).abs() < 1e-9);
        assert!((fade_gain(4, 198.0, 200.0) - 0.5).abs() < 1e-9);
        // a song shorter than four fades is left alone
        assert_eq!(fade_gain(10, 1.0, 30.0), 1.0);
    }

    #[test]
    fn stars_show_the_rating() {
        assert_eq!(stars(0), "");
        assert_eq!(stars(3), "★★★");
        assert_eq!(stars(9), "★★★★★");
    }
}
