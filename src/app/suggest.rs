//! Suggested fixes: songs that lack tags or a cover are looked up online, and what turns up is
//! offered in a card on the Now playing page, one song at a time, to apply or turn down.
use super::{Skratch, listen::now_secs};
use crate::{service::Command, style};
use eframe::egui::{self, Color32, CornerRadius, Rect, RichText, Sense, Stroke, pos2, vec2};
use skratch::{
    model::{SUGGEST_APPLIED, SUGGEST_NEVER, SUGGEST_NOTHING, Track},
    online,
    suggest::{self, Choice},
};
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

/// Most answers kept for songs that have not been played yet.
const KEEP: usize = 200;

/// How long the background lookups wait between songs.
const BETWEEN: Duration = Duration::from_secs(2);

enum Done {
    Found(i64, Result<Vec<Choice>, String>),
    Cover(i64, Result<Vec<u8>, String>),
}

struct Ready {
    choices: Vec<Choice>,
    /// The choice on show.
    shown: usize,
    textures: Vec<Option<egui::TextureHandle>>,
}

/// How much of a choice to take.
#[derive(Clone, Copy, PartialEq)]
enum Take {
    All,
    Tags,
    Cover,
}

enum Answer {
    Apply(Take),
    Skip,
    Never,
}

pub struct Suggest {
    jobs: mpsc::Sender<Track>,
    tx: mpsc::Sender<Done>,
    rx: mpsc::Receiver<Done>,
    ready: HashMap<i64, Ready>,
    /// Songs looked up (or being looked up) this session, and skipped ones: not asked twice.
    asked: HashSet<i64>,
    in_flight: usize,
    failures: u32,
    /// After the services failed to answer, nothing is asked until then.
    pause_until: Option<Instant>,
    last_sweep: Instant,
    /// Songs looked up in the background this session.
    pub looked_up: usize,
}

impl Suggest {
    pub fn new(wake: Arc<dyn Fn() + Send + Sync>) -> Self {
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
            ready: HashMap::new(),
            asked: HashSet::new(),
            in_flight: 0,
            failures: 0,
            pause_until: None,
            last_sweep: Instant::now(),
            looked_up: 0,
        }
    }

    fn paused(&self) -> bool {
        self.pause_until.is_some_and(|until| Instant::now() < until)
    }
}

impl Skratch {
    /// Collects finished lookups and starts the next: the song playing first, then (if allowed)
    /// the one lined up next and the rest of the library, one at a time.
    pub(super) fn suggest_tick(&mut self, ctx: &egui::Context) {
        let now = now_secs();
        while let Ok(done) = self.suggest.rx.try_recv() {
            match done {
                Done::Found(id, answer) => {
                    self.suggest.in_flight = self.suggest.in_flight.saturating_sub(1);
                    match answer {
                        Ok(choices) => {
                            self.suggest.failures = 0;
                            if choices.is_empty() {
                                self.mark_suggested(id, SUGGEST_NOTHING);
                            } else {
                                let textures = choices.iter().map(|_| None).collect();
                                self.suggest.ready.insert(
                                    id,
                                    Ready {
                                        choices,
                                        shown: 0,
                                        textures,
                                    },
                                );
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
            || self.suggest.ready.len() >= KEEP
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

    fn answer_suggestion(&mut self, track: &Track, answer: Answer) {
        let Some(ready) = self.suggest.ready.remove(&track.id) else {
            return;
        };
        match answer {
            Answer::Skip => {}
            Answer::Never => self.mark_suggested(track.id, SUGGEST_NEVER),
            Answer::Apply(take) => {
                let choice = &ready.choices[ready.shown];
                if take != Take::Cover
                    && let Some(edit) = choice.edit_for(track)
                {
                    self.command(Command::Edit(track.id, edit));
                }
                if take != Take::Tags
                    && let Some(cover) = &choice.cover
                {
                    let (url, id, tx, wake) = (
                        cover.full.clone(),
                        track.id,
                        self.suggest.tx.clone(),
                        self.wake.clone(),
                    );
                    std::thread::spawn(move || {
                        let bytes = online::fetch_image(&url).map_err(|e| format!("{e:#}"));
                        let _ = tx.send(Done::Cover(id, bytes));
                        wake();
                    });
                }
                self.mark_suggested(track.id, SUGGEST_APPLIED);
            }
        }
    }

    /// The card in the corner of the Now playing page: what was found online for the song, with
    /// buttons to take it. It keeps its place and size; only its contents change.
    pub(super) fn suggest_card(&mut self, ui: &mut egui::Ui, track: &Track, rect: Rect) {
        let accent = self.accent;
        let Some(ready) = self.suggest.ready.get_mut(&track.id) else {
            return;
        };
        let count = ready.choices.len();
        ready.shown = ready.shown.min(count - 1);
        let shown = ready.shown;
        if ready.textures[shown].is_none()
            && let Some(image) = &ready.choices[shown].thumb
        {
            ready.textures[shown] = Some(ui.ctx().load_texture(
                format!("suggest-{}-{shown}", track.id),
                image.clone(),
                egui::TextureOptions::LINEAR,
            ));
        }
        let choice = &ready.choices[shown];
        let texture = ready.textures[shown].clone();
        let (has_tags, has_cover) = (choice.edit.is_some(), choice.cover.is_some());
        let mut step = 0_i32;
        let mut answer = None;
        let radius = CornerRadius::same(style::RADIUS);
        ui.painter()
            .rect_filled(rect, radius, style::PANEL.gamma_multiply(0.94));
        ui.painter().rect_stroke(
            rect,
            radius,
            Stroke::new(1.0_f32, style::EDGE),
            egui::StrokeKind::Inside,
        );
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
                ui.vertical(|ui| {
                    let line = |ui: &mut egui::Ui, text: String| {
                        ui.add(
                            egui::Label::new(RichText::new(text).size(12.0).color(style::CREAM))
                                .truncate(),
                        );
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
                });
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
                    .on_hover_text("Not now. Skratch asks again next time it starts.")
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
            if let Some(ready) = self.suggest.ready.get_mut(&track.id) {
                ready.shown = at;
            }
        }
        if let Some(answer) = answer {
            self.answer_suggestion(track, answer);
        }
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
