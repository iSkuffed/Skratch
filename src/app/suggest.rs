//! Suggested fixes: songs that lack tags or a cover are looked up online, and what turns up is
//! offered in a card on the Now playing page, one song at a time, to apply or turn down.
use super::{Skratch, listen::now_secs};
use crate::{service::Command, style};
use eframe::egui::{self, Color32, CornerRadius, Rect, RichText, Sense, Stroke, pos2, vec2};
use skratch::{
    model::{SUGGEST_APPLIED, SUGGEST_NEVER, SUGGEST_NOTHING, Track},
    online,
    suggest::{self, Applied, Choice, Store},
};
use std::{
    collections::{HashMap, HashSet},
    path::Path,
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

/// Most answers kept for songs that have not been played yet.
const KEEP: usize = 200;

/// How long the background lookups wait between songs.
const BETWEEN: Duration = Duration::from_secs(2);

enum Done {
    Found(i64, Result<Vec<Choice>, String>),
    /// Covers downloaded one after the other: song, picture or what went wrong.
    Cover(i64, Result<Vec<u8>, String>),
    /// The small picture of one choice's cover (song, choice number).
    Thumb(i64, usize, Option<egui::ColorImage>),
}

/// What the card shows of an answer: which choice, and its pictures.
struct Shown {
    choice: usize,
    textures: Vec<Option<egui::TextureHandle>>,
    thumb_asked: Vec<bool>,
}

impl Shown {
    fn new(count: usize) -> Self {
        Self {
            choice: 0,
            textures: vec![None; count],
            thumb_asked: vec![false; count],
        }
    }
}

/// How much of a choice to take.
#[derive(Clone, Copy, PartialEq)]
pub(super) enum Take {
    All,
    Tags,
    Cover,
}

pub(super) enum Answer {
    Apply(Take),
    Skip,
    Never,
}

pub struct Suggest {
    jobs: mpsc::Sender<Track>,
    tx: mpsc::Sender<Done>,
    rx: mpsc::Receiver<Done>,
    /// Answers waiting for the user (kept between runs) and fixes that can be undone.
    store: Store,
    shown: HashMap<i64, Shown>,
    /// Songs looked up (or being looked up) this session: not asked twice.
    asked: HashSet<i64>,
    /// Songs whose card was put away with "Skip" (they stay in Tidy up).
    skipped: HashSet<i64>,
    /// The song whose fix was just applied: its card offers Undo.
    just_applied: Option<i64>,
    /// Whether answers from an earlier run were checked against the library.
    pruned: bool,
    in_flight: usize,
    /// Songs whose lookup has been sent and not answered yet.
    pending: HashSet<i64>,
    failures: u32,
    /// After the services failed to answer, nothing is asked until then.
    pause_until: Option<Instant>,
    last_sweep: Instant,
    /// Songs looked up in the background this session.
    pub looked_up: usize,
    /// The Tidy up page shows the applied fixes instead of the waiting ones.
    pub show_applied: bool,
}

impl Suggest {
    pub fn new(wake: Arc<dyn Fn() + Send + Sync>, data_dir: &Path) -> Self {
        let (jobs, job_rx) = mpsc::channel::<Track>();
        let (tx, rx) = mpsc::channel();
        let done = tx.clone();
        // one song at a time, so the services' rate limits are kept
        std::thread::spawn(move || {
            while let Ok(track) = job_rx.recv() {
                let found = suggest::find(&track).map_err(|e| format!("{e:#}"));
                if done.send(Done::Found(track.id, found)).is_err() {
                    break;
                }
                wake();
            }
        });
        Self {
            jobs,
            tx,
            rx,
            store: Store::load(data_dir),
            shown: HashMap::new(),
            asked: HashSet::new(),
            skipped: HashSet::new(),
            just_applied: None,
            pruned: false,
            in_flight: 0,
            pending: HashSet::new(),
            failures: 0,
            pause_until: None,
            last_sweep: Instant::now(),
            looked_up: 0,
            show_applied: false,
        }
    }

    fn paused(&self) -> bool {
        self.pause_until.is_some_and(|until| Instant::now() < until)
    }

    /// Whether this song is being looked up right now.
    pub fn pending(&self, id: i64) -> bool {
        self.pending.contains(&id)
    }

    /// How many songs have fixes waiting.
    pub fn waiting(&self) -> usize {
        self.store.waiting.len()
    }
}

impl Skratch {
    fn save_suggestions(&mut self) {
        let dir = self.data_dir.clone();
        self.suggest.store.save(&dir);
    }

    /// Checks the answers kept from an earlier run against the library once it is known: songs
    /// that are gone, or already fixed by hand, are dropped; the rest are not looked up again.
    fn prune_suggestions(&mut self) {
        let (tracks, by_id) = (&self.tracks, &self.by_id);
        let track = |id: &i64| by_id.get(id).map(|i| &tracks[*i]);
        let waiting = &mut self.suggest.store.waiting;
        waiting.retain(|id, choices| {
            let Some(track) = track(id) else {
                return false;
            };
            choices.retain(|c| c.useful_for(track));
            !choices.is_empty() && !matches!(track.suggest, SUGGEST_APPLIED | SUGGEST_NEVER)
        });
        self.suggest
            .store
            .applied
            .retain(|a| by_id.contains_key(&a.id));
        let ids: Vec<i64> = self.suggest.store.waiting.keys().copied().collect();
        for id in ids {
            let count = self.suggest.store.waiting[&id].len();
            self.suggest.shown.insert(id, Shown::new(count));
            self.suggest.asked.insert(id);
        }
        self.suggest.pruned = true;
        self.save_suggestions();
    }

    /// Collects finished lookups and starts the next: the song playing first, then (if allowed)
    /// the one lined up next and the rest of the library, one at a time.
    pub(super) fn suggest_tick(&mut self, ctx: &egui::Context) {
        let now = now_secs();
        if !self.suggest.pruned && !self.tracks.is_empty() {
            self.prune_suggestions();
        }
        if self
            .suggest
            .just_applied
            .is_some_and(|id| self.current().is_none_or(|t| t.id != id))
        {
            self.suggest.just_applied = None;
        }
        while let Ok(done) = self.suggest.rx.try_recv() {
            match done {
                Done::Found(id, answer) => {
                    self.suggest.in_flight = self.suggest.in_flight.saturating_sub(1);
                    self.suggest.pending.remove(&id);
                    match answer {
                        Ok(choices) => {
                            self.suggest.failures = 0;
                            if choices.is_empty() {
                                self.mark_suggested(id, SUGGEST_NOTHING);
                            } else {
                                self.suggest.shown.insert(id, Shown::new(choices.len()));
                                self.suggest.store.waiting.insert(id, choices);
                                self.save_suggestions();
                            }
                        }
                        Err(_) => {
                            // offline or blocked: try this song again later, but not in a rush
                            self.suggest.asked.remove(&id);
                            self.suggest.failures += 1;
                            let minutes = u64::from(self.suggest.failures.min(10));
                            self.suggest.pause_until =
                                Some(Instant::now() + Duration::from_secs(60 * minutes));
                        }
                    }
                }
                Done::Cover(id, Ok(bytes)) => {
                    self.command(Command::SetCover(vec![id], bytes, true))
                }
                Done::Cover(_, Err(error)) => {
                    self.message(format!("Could not download the cover: {error}"));
                }
                Done::Thumb(id, at, image) => {
                    if let Some(choice) = self
                        .suggest
                        .store
                        .waiting
                        .get_mut(&id)
                        .and_then(|choices| choices.get_mut(at))
                    {
                        choice.thumb = image;
                    }
                }
            }
        }
        if self.state.suggest_off || self.suggest.paused() {
            return;
        }
        let playing = self.current().filter(|t| suggest::wanted(t, now)).cloned();
        if let Some(track) = playing.filter(|t| !self.suggest.asked.contains(&t.id)) {
            self.look_up(track);
        }
        if self.state.suggest_sweep_off
            || self.suggest.in_flight > 0
            || self.fetch.is_some()
            || self.suggest.store.waiting.len() >= KEEP
            || self.suggest.last_sweep.elapsed() < BETWEEN
        {
            return;
        }
        self.suggest.last_sweep = Instant::now();
        let fit = |t: &&Track| suggest::wanted(t, now) && !self.suggest.asked.contains(&t.id);
        let next = self
            .state
            .queue
            .peek_next()
            .and_then(|id| self.by_id.get(&id))
            .map(|i| &self.tracks[*i])
            .filter(fit)
            .or_else(|| self.tracks.iter().find(fit))
            .cloned();
        if let Some(track) = next {
            self.suggest.looked_up += 1;
            self.look_up(track);
        }
        // the sweep goes on by itself even when nothing else repaints the window
        ctx.request_repaint_after(BETWEEN);
    }

    fn look_up(&mut self, track: Track) {
        self.suggest.asked.insert(track.id);
        self.suggest.pending.insert(track.id);
        self.suggest.in_flight += 1;
        let _ = self.suggest.jobs.send(track);
    }

    /// Notes the decision on the song here and in the library.
    fn mark_suggested(&mut self, id: i64, state: u8) {
        let now = now_secs();
        if let Some(&i) = self.by_id.get(&id) {
            self.tracks[i].suggest = state;
            self.tracks[i].suggest_at = Some(now);
        }
        self.command(Command::Suggested(vec![id], state, now));
    }

    /// Downloads covers one after the other (so many applied at once are not many downloads at
    /// once); each one is embedded as it arrives.
    fn fetch_covers(&self, covers: Vec<(i64, String)>) {
        if covers.is_empty() {
            return;
        }
        let (tx, wake) = (self.suggest.tx.clone(), self.wake.clone());
        std::thread::spawn(move || {
            for (id, url) in covers {
                let bytes = online::fetch_image(&url).map_err(|e| format!("{e:#}"));
                if tx.send(Done::Cover(id, bytes)).is_err() {
                    break;
                }
                wake();
            }
        });
    }

    /// Takes the answer for one song. Returns the cover to download, if one was chosen.
    fn take_answer(&mut self, id: i64, answer: Answer) -> Option<(i64, String)> {
        let track = self.by_id.get(&id).map(|i| self.tracks[*i].clone())?;
        match answer {
            Answer::Skip => {
                self.suggest.skipped.insert(id);
                None
            }
            Answer::Never => {
                self.suggest.store.waiting.remove(&id);
                self.suggest.shown.remove(&id);
                self.save_suggestions();
                self.mark_suggested(id, SUGGEST_NEVER);
                None
            }
            Answer::Apply(take) => {
                let choices = self.suggest.store.waiting.remove(&id)?;
                let at = self.suggest.shown.remove(&id).map_or(0, |s| s.choice);
                let mut choice = choices.into_iter().nth(at)?;
                choice.thumb = None;
                let tags = take != Take::Cover && !choice.changes.is_empty();
                let cover = take != Take::Tags && choice.cover.is_some();
                if tags && let Some(edit) = choice.edit_for(&track) {
                    self.command(Command::Edit(id, edit));
                }
                let url = cover.then(|| choice.cover.as_ref().map(|c| (id, c.full.clone())));
                let label = format!(
                    "{} - {}",
                    if track.artist.is_empty() {
                        "Unknown artist"
                    } else {
                        &track.artist
                    },
                    track.title
                );
                self.suggest.store.applied.retain(|a| a.id != id);
                self.suggest.store.applied.push(Applied {
                    id,
                    label,
                    choice,
                    tags,
                    cover,
                });
                self.suggest.just_applied = Some(id);
                self.save_suggestions();
                self.mark_suggested(id, SUGGEST_APPLIED);
                url.flatten()
            }
        }
    }

    fn answer_suggestion(&mut self, id: i64, answer: Answer) {
        if let Some(cover) = self.take_answer(id, answer) {
            self.fetch_covers(vec![cover]);
        }
    }

    /// Applies the first answer of every song waiting (after the user confirmed).
    pub(super) fn apply_all_suggestions(&mut self) {
        let ids: Vec<i64> = self.suggest.store.waiting.keys().copied().collect();
        let covers: Vec<(i64, String)> = ids
            .into_iter()
            .filter_map(|id| self.take_answer(id, Answer::Apply(Take::All)))
            .collect();
        self.fetch_covers(covers);
    }

    /// Puts a song back as it was before its fix, and never suggests anything for it again.
    pub(super) fn undo_suggestion(&mut self, id: i64) {
        let Some(at) = self.suggest.store.applied.iter().position(|a| a.id == id) else {
            return;
        };
        let applied = self.suggest.store.applied.remove(at);
        if self.suggest.just_applied == Some(id) {
            self.suggest.just_applied = None;
        }
        if let Some(track) = self.by_id.get(&id).map(|i| self.tracks[*i].clone()) {
            if applied.tags
                && let Some(edit) = applied.choice.undo_for(&track)
            {
                self.command(Command::Edit(id, edit));
            }
            if applied.cover {
                self.command(Command::RemoveCover(vec![id]));
            }
        }
        self.save_suggestions();
        self.mark_suggested(id, SUGGEST_NEVER);
    }

    /// The panel of a card: its background and edge.
    fn card_frame(ui: &egui::Ui, rect: Rect) {
        let radius = CornerRadius::same(style::RADIUS);
        ui.painter()
            .rect_filled(rect, radius, style::PANEL.gamma_multiply(0.94));
        ui.painter().rect_stroke(
            rect,
            radius,
            Stroke::new(1.0_f32, style::EDGE),
            egui::StrokeKind::Inside,
        );
    }

    /// The card in the corner of the Now playing page: what was found online for the song, with
    /// buttons to take it (or, right after, to undo it). It keeps its place and size; only its
    /// contents change.
    pub(super) fn suggest_card(&mut self, ui: &mut egui::Ui, track: &Track, rect: Rect) {
        if self.suggest.just_applied == Some(track.id)
            && !self.suggest.store.waiting.contains_key(&track.id)
        {
            self.applied_card(ui, track, rect);
            return;
        }
        if self.suggest.skipped.contains(&track.id) {
            return;
        }
        let accent = self.accent;
        let Some(choices) = self.suggest.store.waiting.get(&track.id) else {
            return;
        };
        let Some(view) = self.suggest.shown.get_mut(&track.id) else {
            return;
        };
        let count = choices.len();
        view.choice = view.choice.min(count - 1);
        let shown = view.choice;
        let choice = &choices[shown];
        if view.textures[shown].is_none()
            && let Some(image) = &choice.thumb
        {
            view.textures[shown] = Some(ui.ctx().load_texture(
                format!("suggest-{}-{shown}", track.id),
                image.clone(),
                egui::TextureOptions::LINEAR,
            ));
        }
        // a picture from an earlier run is fetched again, once
        if view.textures[shown].is_none()
            && !view.thumb_asked[shown]
            && let Some(cover) = choice.cover.clone()
        {
            view.thumb_asked[shown] = true;
            let (tx, wake, id) = (self.suggest.tx.clone(), self.wake.clone(), track.id);
            std::thread::spawn(move || {
                let _ = tx.send(Done::Thumb(id, shown, suggest::thumbnail(&cover)));
                wake();
            });
        }
        let texture = view.textures[shown].clone();
        let (has_tags, has_cover) = (!choice.changes.is_empty(), choice.cover.is_some());
        let mut step = 0_i32;
        let mut answer = None;
        Self::card_frame(ui, rect);
        let inner = rect.shrink2(vec2(12.0, 8.0));
        ui.scope_builder(egui::UiBuilder::new().max_rect(inner), |ui| {
            ui.set_clip_rect(inner);
            ui.set_width(inner.width());
            ui.spacing_mut().item_spacing = vec2(6.0, 4.0);
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("SUGGESTED FIX")
                        .size(10.5)
                        .color(style::CREAM_DIM),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if count > 1
                        && chevron(ui, Way::Right)
                            .on_hover_text("Next match")
                            .clicked()
                    {
                        step = 1;
                    }
                    ui.label(
                        RichText::new(format!("{}/{count}", shown + 1))
                            .size(12.0)
                            .color(style::CREAM),
                    );
                    if count > 1
                        && chevron(ui, Way::Left)
                            .on_hover_text("Previous match")
                            .clicked()
                    {
                        step = -1;
                    }
                });
            });
            ui.horizontal_top(|ui| {
                if let Some(texture) = &texture {
                    ui.add(
                        egui::Image::new((texture.id(), vec2(58.0, 58.0)))
                            .corner_radius(CornerRadius::same(6)),
                    );
                }
                ui.vertical(|ui| change_lines(ui, choice));
            });
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                let apply = ui.add(
                    egui::Button::new(RichText::new("Apply").color(Color32::BLACK)).fill(accent),
                );
                if apply
                    .on_hover_text("Write these tags and the cover into the song's file")
                    .clicked()
                {
                    answer = Some(Answer::Apply(Take::All));
                }
                let more = chevron(ui, Way::Down);
                egui::Popup::menu(&more).show(|ui| {
                    if ui
                        .add_enabled(has_tags, egui::Button::new("Just the tags"))
                        .clicked()
                    {
                        answer = Some(Answer::Apply(Take::Tags));
                    }
                    if ui
                        .add_enabled(has_cover, egui::Button::new("Just the cover"))
                        .clicked()
                    {
                        answer = Some(Answer::Apply(Take::Cover));
                    }
                });
                if ui
                    .button("Skip")
                    .on_hover_text("Not now. It waits in Tidy up, under Suggestions.")
                    .clicked()
                {
                    answer = Some(Answer::Skip);
                }
                if ui
                    .button("Never")
                    .on_hover_text("Never suggest anything for this song")
                    .clicked()
                {
                    answer = Some(Answer::Never);
                }
            });
        });
        if step != 0 {
            let at = (shown as i32 + step).rem_euclid(count as i32) as usize;
            if let Some(view) = self.suggest.shown.get_mut(&track.id) {
                view.choice = at;
            }
        }
        if let Some(answer) = answer {
            self.answer_suggestion(track.id, answer);
        }
    }

    /// The card right after a fix was applied: what was done, and a way to take it back.
    fn applied_card(&mut self, ui: &mut egui::Ui, track: &Track, rect: Rect) {
        let Some(applied) = self.suggest.store.applied.iter().find(|a| a.id == track.id) else {
            return;
        };
        let mut undo = false;
        let mut done = false;
        Self::card_frame(ui, rect);
        let inner = rect.shrink2(vec2(12.0, 8.0));
        ui.scope_builder(egui::UiBuilder::new().max_rect(inner), |ui| {
            ui.set_clip_rect(inner);
            ui.set_width(inner.width());
            ui.spacing_mut().item_spacing = vec2(6.0, 4.0);
            ui.label(
                RichText::new("FIX APPLIED")
                    .size(10.5)
                    .color(style::CREAM_DIM),
            );
            ui.vertical(|ui| {
                if applied.tags {
                    change_lines(ui, &applied.choice);
                } else if let Some(cover) = &applied.choice.cover {
                    ui.add(
                        egui::Label::new(
                            RichText::new(format!("Cover: {}", cover.label))
                                .size(12.0)
                                .color(style::CREAM),
                        )
                        .truncate(),
                    );
                }
            });
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                if ui
                    .button("Undo")
                    .on_hover_text(
                        "Put the song back as it was. Skratch will not suggest for it again.",
                    )
                    .clicked()
                {
                    undo = true;
                }
                if ui.button("Done").clicked() {
                    done = true;
                }
            });
        });
        if undo {
            self.undo_suggestion(track.id);
        } else if done {
            self.suggest.just_applied = None;
        }
    }

    /// Tidy up, "Suggestions": every fix waiting for an answer (to go through in bulk), and the
    /// applied ones to undo.
    pub(super) fn suggestions_page(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.selectable_value(
                &mut self.suggest.show_applied,
                false,
                format!("Waiting ({})", self.suggest.store.waiting.len()),
            );
            ui.selectable_value(
                &mut self.suggest.show_applied,
                true,
                format!("Applied ({})", self.suggest.store.applied.len()),
            );
        });
        if self.suggest.show_applied {
            self.applied_list(ui);
        } else {
            self.waiting_list(ui);
        }
    }

    fn waiting_list(&mut self, ui: &mut egui::Ui) {
        let mut rows: Vec<(String, i64)> = self
            .suggest
            .store
            .waiting
            .keys()
            .filter_map(|id| self.by_id.get(id).map(|i| (&self.tracks[*i], *id)))
            .map(|(t, id)| (format!("{} - {}", t.artist, t.title).to_lowercase(), id))
            .collect();
        rows.sort();
        if rows.is_empty() {
            ui.add_space(20.0);
            ui.label(
                "Nothing is waiting. While you listen, Skratch looks up songs that lack an album, year, track number or cover, and what it finds waits here (and on the Now playing page) until you apply or turn it down. The checkboxes under Settings turn this off.",
            );
            return;
        }
        let mut apply_all = false;
        ui.horizontal(|ui| {
            ui.label(match rows.len() {
                1 => "1 song has a fix waiting.".to_owned(),
                n => format!("{n} songs have a fix waiting."),
            });
            apply_all = super::listen::confirm_button(
                ui,
                "Apply all",
                &format!(
                    "This writes the first suggestion for each of the {} songs into its file (tags, and the cover). Apply them all?",
                    rows.len()
                ),
                "Yes, apply all",
            );
        });
        ui.separator();
        let mut answers: Vec<(i64, Answer)> = Vec::new();
        let mut next: Vec<(i64, i32)> = Vec::new();
        let height = 46.0;
        style::scroll()
            .id_salt("tidy-suggest")
            .show_rows(ui, height, rows.len(), |ui, range| {
                for (_, id) in &rows[range] {
                    let Some(track) = self.by_id.get(id).map(|i| &self.tracks[*i]) else {
                        continue;
                    };
                    let choices = &self.suggest.store.waiting[id];
                    let at = self
                        .suggest
                        .shown
                        .get(id)
                        .map_or(0, |s| s.choice)
                        .min(choices.len() - 1);
                    let choice = &choices[at];
                    ui.allocate_ui_with_layout(
                        vec2(ui.available_width(), height - 4.0),
                        egui::Layout::left_to_right(egui::Align::Center),
                        |ui| {
                            if ui.button("Apply").clicked() {
                                answers.push((*id, Answer::Apply(Take::All)));
                            }
                            if ui
                                .button("Never")
                                .on_hover_text("Never suggest anything for this song")
                                .clicked()
                            {
                                answers.push((*id, Answer::Never));
                            }
                            if choices.len() > 1
                                && ui
                                    .small_button(format!("{}/{}", at + 1, choices.len()))
                                    .on_hover_text("Show the next match")
                                    .clicked()
                            {
                                next.push((*id, 1));
                            }
                            ui.vertical(|ui| {
                                ui.spacing_mut().item_spacing.y = 1.0;
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(format!(
                                            "{} - {}",
                                            track.artist, track.title
                                        ))
                                        .color(style::CREAM),
                                    )
                                    .truncate(),
                                );
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(summary(choice))
                                            .size(12.0)
                                            .color(style::CREAM_DIM),
                                    )
                                    .truncate(),
                                );
                            });
                        },
                    );
                }
            });
        for (id, step) in next {
            if let Some(view) = self.suggest.shown.get_mut(&id) {
                let count = self.suggest.store.waiting[&id].len() as i32;
                view.choice = (view.choice as i32 + step).rem_euclid(count) as usize;
            }
        }
        for (id, answer) in answers {
            self.answer_suggestion(id, answer);
        }
        if apply_all {
            self.apply_all_suggestions();
        }
    }

    fn applied_list(&mut self, ui: &mut egui::Ui) {
        if self.suggest.store.applied.is_empty() {
            ui.add_space(20.0);
            ui.label(
                "No fix has been applied yet. The last few you apply can be undone from here.",
            );
            return;
        }
        let rows: Vec<(i64, String, String)> = self
            .suggest
            .store
            .applied
            .iter()
            .rev()
            .map(|a| (a.id, a.label.clone(), summary(&a.choice)))
            .collect();
        let mut undo = None;
        let height = 46.0;
        style::scroll()
            .id_salt("tidy-applied")
            .show_rows(ui, height, rows.len(), |ui, range| {
                for (id, label, text) in &rows[range] {
                    ui.allocate_ui_with_layout(
                        vec2(ui.available_width(), height - 4.0),
                        egui::Layout::left_to_right(egui::Align::Center),
                        |ui| {
                            if ui
                                .button("Undo")
                                .on_hover_text("Put the song back as it was. Skratch will not suggest for it again.")
                                .clicked()
                            {
                                undo = Some(*id);
                            }
                            ui.vertical(|ui| {
                                ui.spacing_mut().item_spacing.y = 1.0;
                                ui.add(egui::Label::new(RichText::new(label).color(style::CREAM)).truncate());
                                ui.add(
                                    egui::Label::new(RichText::new(text).size(12.0).color(style::CREAM_DIM))
                                        .truncate(),
                                );
                            });
                        },
                    );
                }
            });
        if let Some(id) = undo {
            self.undo_suggestion(id);
        }
    }
}

/// What a choice changes, on one line.
fn summary(choice: &Choice) -> String {
    let mut parts: Vec<String> = choice
        .changes
        .iter()
        .map(|c| format!("{}: {}", c.field, c.to))
        .collect();
    if choice.cover.is_some() {
        parts.push("Cover".to_owned());
    }
    parts.join("  ·  ")
}

/// The lines of a card: each change, and the cover.
fn change_lines(ui: &mut egui::Ui, choice: &Choice) {
    let line = |ui: &mut egui::Ui, text: String| {
        ui.add(egui::Label::new(RichText::new(text).size(12.0).color(style::CREAM)).truncate());
    };
    for change in choice.changes.iter().take(4) {
        line(
            ui,
            if change.from.is_empty() {
                format!("{}: {}", change.field, change.to)
            } else {
                format!("{}: {} → {}", change.field, change.from, change.to)
            },
        );
    }
    if let Some(cover) = &choice.cover {
        line(ui, format!("Cover: {}", cover.label));
    }
}

#[derive(Clone, Copy)]
enum Way {
    Left,
    Right,
    Down,
}

/// A small square button with an arrow head drawn on it, so no font has to have the glyph.
fn chevron(ui: &mut egui::Ui, way: Way) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(22.0, 24.0), Sense::click());
    let colour = if response.hovered() {
        style::CREAM
    } else {
        style::CREAM_DIM
    };
    if response.hovered() {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(5), style::RAISED);
    }
    let c = rect.center();
    let points = match way {
        Way::Left => [
            pos2(c.x + 2.5, c.y - 4.5),
            pos2(c.x - 2.5, c.y),
            pos2(c.x + 2.5, c.y + 4.5),
        ],
        Way::Right => [
            pos2(c.x - 2.5, c.y - 4.5),
            pos2(c.x + 2.5, c.y),
            pos2(c.x - 2.5, c.y + 4.5),
        ],
        Way::Down => [
            pos2(c.x - 4.5, c.y - 2.0),
            pos2(c.x, c.y + 3.0),
            pos2(c.x + 4.5, c.y - 2.0),
        ],
    };
    ui.painter().add(egui::Shape::line(
        points.to_vec(),
        Stroke::new(1.6_f32, colour),
    ));
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}
