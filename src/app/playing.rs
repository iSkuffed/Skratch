//! The "Now playing" page: a big spinning record next to a panel the user can switch
//! between the track details, the queue and the whole library.
use super::{Action, Skratch, display, lyricview, matches_query};
use crate::{
    state::{NowPanel, RhymeShow, Source, VizStyle},
    style,
};
use eframe::egui::emath::easing::cubic_out;
use eframe::egui::text::{CCursor, CCursorRange};
use eframe::egui::{self, Align, Pos2, Rect, RichText, Sense, Vec2, pos2, vec2};
use skratch::{
    lyricfx,
    lyrics::{self, Lyrics, OtherState, Status},
    model::Track,
};
use std::time::Duration;

/// How long the settings drawer stays out once the pointer has left it (or the window has lost
/// focus) before it slides away, in seconds.
const DRAWER_LINGER: f64 = 1.2;

/// Half the side of the square the settings gear reacts to the pointer in.
const GEAR_HALF: f32 = 14.0;

/// How wide the pull tab holding the gear is.
const TAB_W: f32 = 38.0;

/// How long the animation when the panel changes tab takes, in seconds.
const REVEAL_SECONDS: f64 = 0.32;

/// The settings drawer that slides out over the right of the panel from its gear.
#[derive(Default)]
pub struct Drawer {
    /// Out or sliding out; false once it has been told to go.
    open: bool,
    /// When (egui seconds) the pointer was last on the gear or the drawer, window focused.
    near_at: f64,
    /// How far out it was drawn last frame, 0 (away) to 1 (out).
    out: f32,
    /// Which of the Lyrics and Visualizer tabs has its quick options slid out in the tab row.
    flyout: Option<NowPanel>,
    /// Whether select mode was on last frame, so turning it on (from a right-click menu) brings
    /// the selection controls out once.
    select_seen: bool,
}

/// One of the quick options that slide out of the tabs.
#[derive(Clone, Copy, PartialEq)]
enum Quick {
    LookUp,
    TryAgain,
    Automatic,
    Type,
    Import,
    Clear,
    Wrong,
    ToggleLyrics,
    Style(VizStyle),
    VizOff,
    FullScreen,
    /// Select mode in the lists: on, or off with nothing selected.
    Select,
    SelectAll,
    SelectNone,
}

impl Quick {
    /// What the short label in the tab row means.
    fn hint(self) -> &'static str {
        match self {
            Self::LookUp => {
                "Look these lyrics up online (sends the artist, title, album and length)"
            }
            Self::TryAgain => "Ask the lyric services again",
            Self::Automatic => "Drop the lyrics typed, imported or cleared and look again",
            Self::Type => "Edit the lyrics shown, or type your own (empty when there are none)",
            Self::Import => "Bring the lyrics in from a .lrc or .txt file",
            Self::Clear => "Wrong lyrics: forget them and stop looking for this track",
            Self::Wrong => {
                "Wrong lyrics: never offer these again for this track, and look for different ones"
            }
            Self::ToggleLyrics => "Show or hide the lyrics",
            Self::Style(_) => "Draw the visualizer this way",
            Self::VizOff => "Turn the visualizer off",
            Self::FullScreen => "Full screen: fill the window with the visualizer. Esc leaves.",
            Self::Select => {
                "Tick songs to act on several at once (right-click one for the actions)"
            }
            Self::SelectAll => "Tick every song in the list",
            Self::SelectNone => "Untick everything",
        }
    }
}

/// The animation when the panel changes tab: from the top down, each part comes in a little
/// after the one above it, and the very top is there from the first frame. Nothing moves, so
/// whatever sits next to the tab that was just clicked never shifts.
#[derive(Clone, Copy)]
pub struct Reveal {
    /// How far along the whole animation is, 0 to 1 (not eased).
    progress: f32,
    /// Where the animated part starts and how tall it is, which is what "down" is measured in.
    top: f32,
    span: f32,
}

impl Reveal {
    pub const DONE: Self = Self {
        progress: 1.0,
        top: 0.0,
        span: 1.0,
    };

    /// The same animation measured over `rect`.
    fn over(self, rect: Rect) -> Self {
        Self {
            top: rect.top(),
            span: rect.height(),
            ..self
        }
    }

    fn done(self) -> bool {
        self.progress >= 1.0
    }

    /// For what is drawn as one piece (the visualizer, the backing behind a list): it fades in
    /// all at once.
    pub fn uniform(self) -> f32 {
        cubic_out(self.progress)
    }

    /// How visible something whose top edge is at `y` is right now. At the top it is always
    /// fully there; lower down it starts later, from less.
    fn at(self, y: f32) -> f32 {
        if self.done() {
            return 1.0;
        }
        let down = ((y - self.top) / self.span.max(1.0)).clamp(0.0, 1.0);
        let local = ((self.progress - 0.4 * down) / (1.0 - 0.4 * down)).clamp(0.0, 1.0);
        let start = 1.0 - down;
        start + (1.0 - start) * cubic_out(local)
    }

    /// Draws `add` as visible as this gives something that starts at the cursor.
    fn show<R>(self, ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
        if self.done() {
            return add(ui);
        }
        let base = ui.opacity();
        ui.set_opacity(base * self.at(ui.cursor().min.y));
        let result = add(ui);
        ui.set_opacity(base);
        result
    }
}

/// The tab row the settings gear rests at the end of.
#[derive(Clone, Copy)]
struct GearRow {
    /// The tabs, as tall as the row.
    tabs: Rect,
    /// The right edge of the panel, where the gear rests when the drawer is put away.
    right: f32,
}

/// Where the settings drawer and its gear go on a page `edge` wide. The gear sits on a small
/// pull tab that is part of the drawer: at the end of the tab row while the drawer is put away,
/// and against the drawer's left edge, on the same row, once it is out.
#[derive(Debug, Clone, Copy, PartialEq)]
struct DrawerLayout {
    /// Clicks here miss what is under the drawer.
    block: Rect,
    /// The dark backing.
    backing: Rect,
    /// The pull tab the gear sits in, and which of its corners are rounded.
    tab: Rect,
    tab_round: egui::CornerRadius,
    /// Where the options are drawn.
    content: Rect,
    /// Where the pointer counts as on the open drawer: all of it, and the tab row beside it.
    near: Rect,
    /// The gear's centre with the drawer put away, and with it out.
    gear_in: Pos2,
    gear_out: Pos2,
}

impl DrawerLayout {
    fn new(edge: Rect, row: GearRow) -> Self {
        let y = row.tabs.center().y;
        let row_top = row.tabs.top().min(y - GEAR_HALF);
        let row_bottom = row.tabs.bottom().max(y + GEAR_HALF);
        // the tab hangs off the drawer's left edge, so the drawer leaves room for it
        let width = (edge.width() * 0.4)
            .clamp(300.0, 420.0)
            .min(edge.width() - TAB_W);
        let left = edge.right() - width;
        let inner = (width - 48.0).max(240.0).min(width - 24.0);
        let content = Rect::from_min_max(
            pos2(edge.right() - 24.0 - inner, row_bottom + 14.0),
            pos2(edge.right() - 24.0, edge.bottom() - 14.0),
        );
        let gear_in = pos2(row.right - GEAR_HALF, y);
        // the drawer runs the whole height of the page and covers the tab row while it is out
        let tab = Rect::from_min_max(pos2(left - TAB_W, row_top), pos2(left, row_bottom));
        let tab_round = egui::CornerRadius {
            nw: 10,
            sw: 10,
            ne: 0,
            se: 0,
        };
        let gear_out = pos2(tab.center().x.min(gear_in.x), y);
        let block = Rect::from_min_max(pos2(left, edge.top()), edge.right_bottom());
        let near = Rect::from_min_max(pos2(tab.left() - 4.0, edge.top()), edge.right_bottom());
        Self {
            block,
            backing: block,
            tab,
            tab_round,
            content,
            near,
            gear_in,
            gear_out,
        }
    }

    /// How far the drawer, and the gear on it, are from where they rest at `out` (0 away, 1 out).
    fn shift(&self, out: f32) -> Vec2 {
        vec2((1.0 - out) * (self.gear_in.x - self.gear_out.x), 0.0)
    }

    fn gear_at(&self, out: f32) -> Pos2 {
        self.gear_out + self.shift(out)
    }
}

/// A section title in the drawer: big, centred, over a thin rule.
fn drawer_title(ui: &mut egui::Ui, text: &str) {
    ui.vertical_centered(|ui| {
        ui.label(RichText::new(text).size(15.0).strong().color(style::CREAM));
    });
    ui.separator();
}

/// Whether the panel view is one of the two lists, which share a page.
fn is_list(panel: NowPanel) -> bool {
    matches!(panel, NowPanel::UpNext | NowPanel::Library)
}

/// Whether two views are the same page, so switching between them does not start the
/// animation over: the two lists, and the Lyrics and Visualizer tabs.
fn same_page(a: NowPanel, b: NowPanel) -> bool {
    (is_list(a) && is_list(b)) || (a.is_stage() && b.is_stage())
}

impl Skratch {
    pub(super) fn playing_ui(&mut self, ui: &mut egui::Ui) {
        egui::CentralPanel::default().show_inside(ui, |ui| {
            if self.state.look.mood {
                style::wash(ui, self.accent);
            }
            ui.set_opacity(self.fade);
            let Some(track) = self.current().cloned() else {
                ui.add_space(24.0);
                ui.heading("Nothing playing");
                ui.label("Pick a track below.");
                self.reveal = Reveal::DONE;
                self.panel_list(ui, NowPanel::Library);
                return;
            };
            let art = track
                .artwork
                .as_deref()
                .and_then(|path| self.art.texture(path));
            let label = style::seed_color(&format!("{}{}", track.artist, track.album));
            self.reveal = self.panel_reveal(ui.ctx());
            let (art_mix, old_art, _) = self.crossfade(ui.ctx());
            let progress = (self.state.position / self.duration.max(1.0)).clamp(0.0, 1.0) as f32;
            let area = ui.available_rect_before_wrap();
            let r = (area.height() * 0.38)
                .min(area.width() * 0.19)
                .clamp(80.0, 200.0);
            let scale = r / 52.0;
            let centre = pos2(area.left() + r * 1.2 + 30.0, area.center().y);

            // the cover as a poster on the right, fading out towards the record
            let side = area.height() * 1.1;
            let poster = egui::Rect::from_min_size(
                pos2(area.right() - side * 0.9, area.top() - side * 0.05),
                vec2(side, side),
            );
            for (texture, opacity) in [(art, art_mix), (old_art, 1.0 - art_mix)] {
                let Some(texture) = texture.filter(|_| opacity > 0.0) else {
                    continue;
                };
                let mut mesh = egui::Mesh::with_texture(texture);
                // three columns: transparent on the left, full tint from 60% across
                for (u, alpha) in [(0.0, 0.0), (0.6, 32.0), (1.0, 32.0)] {
                    let x = egui::lerp(poster.x_range(), u);
                    for v in [0.0, 1.0] {
                        mesh.vertices.push(egui::epaint::Vertex {
                            pos: pos2(x, egui::lerp(poster.y_range(), v)),
                            uv: pos2(u, v),
                            color: egui::Color32::from_white_alpha((alpha * opacity) as u8),
                        });
                    }
                }
                for i in [0, 2] {
                    mesh.add_triangle(i, i + 1, i + 2);
                    mesh.add_triangle(i + 1, i + 2, i + 3);
                }
                ui.painter()
                    .with_clip_rect(area)
                    .add(egui::Shape::mesh(mesh));
            }

            // the ring style circles the record itself, under everything else drawn on it
            if self.ring_on_record() {
                self.viz_ring_at(ui, centre, r);
            }
            // a faint halo in the track's colour that swells with the bass
            let beat = self.beat;
            for (grow, alpha, reach) in [(1.4, 0.04, 0.22), (1.25, 0.05, 0.13), (1.1, 0.07, 0.06)] {
                ui.painter().circle_filled(
                    centre,
                    r * (grow + reach * beat),
                    self.accent.gamma_multiply(alpha + 0.04 * beat),
                );
            }
            let hit = egui::Rect::from_center_size(centre, vec2(2.0 * r, 2.0 * r));
            let response = ui
                .interact(hit, ui.id().with("record"), Sense::click())
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .on_hover_text("Click to play or pause");
            if response.clicked() {
                self.action = Some(Action::Pause);
            }
            if self.state.look.depth {
                style::shadow_disc(ui.painter(), centre, r);
            }
            style::record_fading(
                ui.painter(),
                centre,
                r,
                art,
                old_art.map(|id| (id, 1.0 - art_mix)),
                label,
                self.spin,
            );
            style::tonearm(
                ui.painter(),
                centre + vec2(92.0, -54.0) * scale,
                self.arm,
                progress,
                self.accent,
                scale,
            );

            // suggested fixes, above the record; hidden when the window is too short for them
            let card_height = (centre.y - r * 1.05 - 18.0 - area.top()).min(190.0);
            if card_height >= 128.0 {
                let card = Rect::from_min_size(
                    pos2(area.left() + 16.0, area.top() + 10.0),
                    vec2(240.0, card_height),
                );
                self.suggest_card(ui, &track, card);
            }

            let left = centre.x + r * 1.9 + 30.0;
            let panel = egui::Rect::from_min_max(
                pos2(left, area.top() + 14.0),
                pos2(area.right() - 24.0, area.bottom() - 14.0),
            );
            ui.scope_builder(egui::UiBuilder::new().max_rect(panel), |ui| {
                ui.set_width(panel.width());
                self.panel_ui(ui, &track, area);
            });
        });
    }

    /// The switchable right-hand panel: a row of views to pick from, then the chosen content.
    /// `edge` is the whole area the page has, which the settings drawer slides out within.
    fn panel_ui(&mut self, ui: &mut egui::Ui, track: &Track, edge: Rect) {
        let panel = self.state.now_panel;
        // the gear rests at the panel's edge, past the tabs (the row's own rect stops at them)
        let right = ui.available_rect_before_wrap().right();
        // the row is a fixed strip, so tabs pushed past its end cannot widen the page under it
        let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), 28.0), Sense::hover());
        let mut header = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(row)
                .layout(egui::Layout::left_to_right(Align::Center)),
        );
        // the drawer covers the tab row while it is out, so the labels leave first
        header.multiply_opacity(1.0 - self.drawer.out);
        let tabs = self.tab_row(&mut header, panel, track, right);
        let gear_row = GearRow {
            tabs: Rect::from_min_max(
                pos2(tabs.left(), row.top()),
                pos2(tabs.right(), row.bottom()),
            ),
            right,
        };
        ui.add_space(4.0);
        let area = ui.available_rect_before_wrap();
        let reveal = self.reveal.over(area);
        self.reveal = reveal;
        match panel {
            NowPanel::Details => self.details(ui, track),
            NowPanel::Lyrics | NowPanel::Visualizer => self.stage(ui, track, area),
            NowPanel::UpNext | NowPanel::Library => {
                // the visualizer keeps running behind the list, under a wash that is darkest
                // at the top and clears towards the end of the list
                if self.state.visualizer.behind_lists
                    && self.state.visualizer.shown
                    && !self.ring_on_record()
                {
                    ui.scope(|ui| {
                        ui.multiply_opacity(reveal.uniform());
                        self.viz_panel(ui, area, 1.0);
                        style::scrim_down(&ui.painter().with_clip_rect(area), area, 10.0, 160);
                    });
                }
                ui.scope(|ui| {
                    ui.multiply_opacity(1.0 - 0.6 * self.drawer.out);
                    self.panel_list(ui, panel);
                });
            }
        }
        // Details has nothing to set, and the lyrics editor has no use for the drawer
        let gear = panel != NowPanel::Details && self.lyrics.editing.is_none();
        self.settings_drawer(ui, track, gear_row, gear, edge);
    }

    /// The row of tabs. Lyrics and Visualizer each slide a few quick options out to their right,
    /// pushing the tabs after them along; one closes when the other opens, when its own tab is
    /// clicked again, or when the click lands elsewhere. Whatever runs past the settings gear is
    /// cut off, so no text ever sits under it. Returns the rectangle the tabs span.
    fn tab_row(&mut self, ui: &mut egui::Ui, panel: NowPanel, track: &Track, right: f32) -> Rect {
        let ctx = ui.ctx().clone();
        let mut clip = ui.clip_rect();
        clip.max.x = clip.max.x.min(right - 2.0 * GEAR_HALF - 8.0);
        ui.set_clip_rect(clip);
        ui.spacing_mut().item_spacing.x = 0.0;
        if self.select_mode != self.drawer.select_seen {
            self.drawer.select_seen = self.select_mode;
            if self.select_mode && matches!(panel, NowPanel::UpNext | NowPanel::Library) {
                self.drawer.flyout = Some(panel);
            }
        }
        if self.drawer.flyout.is_some_and(|open| open != panel) {
            self.drawer.flyout = None;
        }
        let (mut tabs, mut hit) = (Rect::NOTHING, Rect::NOTHING);
        let (mut clicked, mut chosen) = (false, None);
        for choice in NowPanel::ALL {
            let active = choice == panel;
            let mut label = choice.title().to_uppercase();
            if choice.has_flyout() {
                label.push_str(" ›");
            }
            let mut text = RichText::new(label).size(12.0).strong().color(if active {
                style::CREAM
            } else {
                style::CREAM_DIM
            });
            if active {
                text = text.underline();
            }
            let tab = ui
                .add(egui::Label::new(text).extend().sense(Sense::click()))
                .on_hover_cursor(egui::CursorIcon::PointingHand);
            tabs = tabs.union(tab.rect);
            hit = hit.union(tab.rect);
            if tab.clicked() {
                clicked = true;
                self.drawer.flyout = if choice.has_flyout() && self.drawer.flyout != Some(choice) {
                    Some(choice)
                } else {
                    None
                };
                if !active {
                    self.state.now_panel = choice;
                    self.listed = None;
                }
            }
            if choice.has_flyout() {
                let open = if self.state.effects.transitions() {
                    ctx.animate_bool_with_time_and_easing(
                        egui::Id::new(("flyout", choice.title())),
                        self.drawer.flyout == Some(choice),
                        0.22,
                        cubic_out,
                    )
                } else {
                    f32::from(self.drawer.flyout == Some(choice))
                };
                if open > 0.0 {
                    let (rect, picked) = self.flyout(ui, choice, track, open);
                    hit = hit.union(rect);
                    chosen = chosen.or(picked);
                }
            }
            ui.add_space(18.0);
        }
        let pressed = ctx.input(|i| i.pointer.primary_pressed());
        let at = ctx.input(|i| i.pointer.interact_pos());
        if pressed && !clicked && at.is_some_and(|at| !hit.expand(4.0).contains(at)) {
            self.drawer.flyout = None;
        }
        if let Some(quick) = chosen {
            self.quick_apply(&ctx, quick, track);
        }
        tabs
    }

    /// `which`'s quick options, `open` of the way out (0 to 1), as pills in the tab row. Returns
    /// the room they take and the one that was clicked.
    fn flyout(
        &self,
        ui: &mut egui::Ui,
        which: NowPanel,
        track: &Track,
        open: f32,
    ) -> (Rect, Option<Quick>) {
        const LEAD: f32 = 8.0;
        const PAD: f32 = 6.0;
        const GAP: f32 = 2.0;
        let items = self.quick_items(which, track);
        let font = egui::FontId::proportional(12.0);
        let galleys: Vec<_> = items
            .iter()
            .map(|(_, text, _)| {
                ui.painter()
                    .layout_no_wrap(text.clone(), font.clone(), style::CREAM)
            })
            .collect();
        let natural = LEAD
            + galleys.iter().map(|g| g.size().x + 2.0 * PAD).sum::<f32>()
            + GAP * (items.len().saturating_sub(1)) as f32;
        let (rect, _) = ui.allocate_exact_size(vec2(natural * open, 28.0), Sense::hover());
        let mut inner = ui.new_child(egui::UiBuilder::new().max_rect(rect));
        inner.set_clip_rect(rect.intersect(ui.clip_rect()));
        inner.multiply_opacity(open);
        let (mut x, mut picked) = (rect.left() + LEAD, None);
        for (n, ((quick, _, selected), galley)) in items.iter().zip(galleys).enumerate() {
            let size = vec2(galley.size().x + 2.0 * PAD, 22.0);
            let item = Rect::from_min_size(pos2(x, rect.center().y - size.y / 2.0), size);
            x += size.x + GAP;
            let response = inner
                .interact(
                    item,
                    egui::Id::new(("quick", which.title(), n)),
                    Sense::click(),
                )
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .on_hover_text(quick.hint());
            let fill = if *selected {
                30
            } else if response.hovered() {
                16
            } else {
                0
            };
            let painter = inner.painter();
            painter.rect_filled(item, 11, egui::Color32::from_white_alpha(fill));
            let colour = if *selected || response.hovered() {
                style::CREAM
            } else {
                style::CREAM_DIM
            };
            painter.galley_with_override_text_color(
                pos2(item.left() + PAD, item.center().y - galley.size().y / 2.0),
                galley,
                colour,
            );
            if response.clicked() {
                picked = Some(*quick);
            }
        }
        (rect, picked)
    }

    /// What Lyrics and Visualizer bring out in the tab row: the label of each, and whether it is
    /// the one in force. Everything here is also in the settings drawer.
    fn quick_items(&self, which: NowPanel, track: &Track) -> Vec<(Quick, String, bool)> {
        let cfg = &self.state.visualizer;
        if matches!(which, NowPanel::UpNext | NowPanel::Library) {
            let count = self.selected.len();
            return if self.select_mode {
                vec![
                    (Quick::Select, "Done".to_owned(), true),
                    (Quick::SelectAll, "All".to_owned(), false),
                    (
                        Quick::SelectNone,
                        if count > 0 {
                            format!("None ({count})")
                        } else {
                            "None".to_owned()
                        },
                        false,
                    ),
                ]
            } else {
                vec![(Quick::Select, "Select".to_owned(), false)]
            };
        }
        if which == NowPanel::Lyrics {
            let dir = self.data_dir.join("lyrics");
            let first = if !self.state.lyrics_online {
                (Quick::LookUp, "Look up")
            } else if lyrics::has_user_version(&dir, track) {
                (Quick::Automatic, "Automatic")
            } else {
                (Quick::TryAgain, "Try again")
            };
            let items = vec![
                (first.0, first.1, false),
                (Quick::Type, "Edit", false),
                (Quick::Import, "Import", false),
                if self.lyrics.online_shown() {
                    (Quick::Wrong, "Wrong", false)
                } else {
                    (Quick::Clear, "Clear", false)
                },
                (
                    Quick::ToggleLyrics,
                    if cfg.lyrics { "Hide" } else { "Show" },
                    !cfg.lyrics,
                ),
            ];
            return items
                .into_iter()
                .map(|(quick, text, on)| (quick, text.to_owned(), on))
                .collect();
        }
        let mut items: Vec<_> = VizStyle::ALL
            .into_iter()
            .map(|s| {
                (
                    Quick::Style(s),
                    s.title().to_owned(),
                    cfg.shown && cfg.style == s,
                )
            })
            .collect();
        items.push((Quick::VizOff, "Off".to_owned(), !cfg.shown));
        items.push((Quick::FullScreen, "Full".to_owned(), false));
        items
    }

    /// Does what a quick option says. The lyrics come back whenever one of their options is used,
    /// so the result is never hidden.
    fn quick_apply(&mut self, ctx: &egui::Context, quick: Quick, track: &Track) {
        let dir = self.data_dir.join("lyrics");
        if matches!(
            quick,
            Quick::LookUp
                | Quick::TryAgain
                | Quick::Automatic
                | Quick::Type
                | Quick::Import
                | Quick::Wrong
        ) {
            self.state.visualizer.lyrics = true;
        }
        match quick {
            Quick::LookUp => self.state.lyrics_online = true,
            Quick::TryAgain => self.lyrics.ask_again(&dir, track),
            Quick::Automatic => {
                lyrics::restore(&dir, track);
                self.lyrics.retry();
            }
            Quick::Type => {
                self.lyrics.editing = Some(match &self.lyrics.status {
                    Status::Found(found) => found.to_text(),
                    _ => String::new(),
                });
            }
            Quick::Import => self.import_lyrics_file(ctx),
            Quick::Clear => self.save_lyrics(&dir, track, ""),
            Quick::Wrong => self.wrong_lyrics(&dir, track),
            Quick::ToggleLyrics => self.state.visualizer.lyrics = !self.state.visualizer.lyrics,
            Quick::Style(style) => {
                self.state.visualizer.style = style;
                self.state.visualizer.shown = true;
            }
            Quick::VizOff => self.state.visualizer.shown = false,
            Quick::FullScreen => self.viz_enter_full(ctx.input(|i| i.time)),
            Quick::Select => {
                self.select_mode = !self.select_mode;
                if !self.select_mode {
                    self.selected.clear();
                }
            }
            Quick::SelectAll => self.selected.extend(self.list_ids.iter().copied()),
            Quick::SelectNone => self.selected.clear(),
        }
    }

    /// The page behind both the Lyrics and the Visualizer tab: the visualizer with the lyrics
    /// over it. Either can be switched off from the tab's options; nothing else tells them apart.
    fn stage(&mut self, ui: &mut egui::Ui, track: &Track, area: Rect) {
        let dir = self.data_dir.join("lyrics");
        // a song with no artist waits for its tag lookup (a few seconds): lyrics are found by artist
        let mut online = self.state.lyric_sources();
        if track.artist.trim().is_empty() && self.suggest.pending(track.id) {
            online.allowed = false;
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_secs(1));
        }
        self.lyrics.update(ui.ctx(), track, &dir, online);
        let cfg = &self.state.visualizer;
        let editing = self.lyrics.editing.is_some();
        let words = cfg.lyrics || editing;
        let over = words && (editing || matches!(self.lyrics.status, Status::Found(_)));
        if cfg.shown && !self.ring_on_record() {
            self.viz_backdrop(ui, area, if over { 0.3 } else { 1.0 });
        }
        if words {
            // the lyrics step back while the drawer is out, so they do not show through it
            ui.scope(|ui| {
                ui.multiply_opacity(1.0 - 0.6 * self.drawer.out);
                self.lyrics(ui, track);
            });
        }
    }

    /// The visualizer filling `area`, fading in with the tab.
    fn viz_backdrop(&mut self, ui: &mut egui::Ui, area: Rect, strength: f32) {
        ui.scope(|ui| {
            ui.multiply_opacity(self.reveal.uniform());
            self.viz_panel(ui, area, strength);
        });
    }

    /// Whether the visualizer's ring is drawn around the record on the left, not in the panel.
    fn ring_on_record(&self) -> bool {
        let cfg = &self.state.visualizer;
        cfg.style == VizStyle::Ring
            && cfg.shown
            && match self.state.now_panel {
                NowPanel::Lyrics | NowPanel::Visualizer => true,
                NowPanel::UpNext | NowPanel::Library => cfg.behind_lists,
                NowPanel::Details => false,
            }
    }

    /// How far the animation for the chosen view has got; starts over whenever the tab changes,
    /// except between Up next and Library, which are one page that swaps its rows.
    fn panel_reveal(&mut self, ctx: &egui::Context) -> Reveal {
        let (panel, now) = (self.state.now_panel, ctx.input(|i| i.time));
        if self.panel_shown != Some(panel) {
            let between_lists = self.panel_shown.is_some_and(|old| same_page(old, panel));
            if !between_lists {
                // the view the page opens on is simply there; only a switch animates
                self.panel_shown_at = if self.panel_shown.is_some() {
                    now
                } else {
                    f64::NEG_INFINITY
                };
            }
            self.panel_shown = Some(panel);
        }
        if !self.state.effects.transitions() {
            return Reveal::DONE;
        }
        let progress = ((now - self.panel_shown_at) / REVEAL_SECONDS).clamp(0.0, 1.0) as f32;
        if progress < 1.0 {
            ctx.request_repaint();
        }
        Reveal {
            progress,
            top: 0.0,
            span: 1.0,
        }
    }

    /// The settings drawer: slides out over the right of the page while the pointer is on the
    /// gear or on it, and back in once the pointer has been away (or the window unfocused) for
    /// a moment. The gear rides on its left edge like a pull tab. It holds the visualizer's
    /// options and everything about the lyrics. `gear` says whether this view has settings.
    fn settings_drawer(
        &mut self,
        ui: &egui::Ui,
        track: &Track,
        row: GearRow,
        gear: bool,
        edge: Rect,
    ) {
        let ctx = ui.ctx().clone();
        let id = egui::Id::new("settings-drawer");
        let (now, focused) = ctx.input(|i| (i.time, i.focused));
        // an open drawer this old was left behind when the page was switched away from
        if self.drawer.open && now - self.drawer.near_at > DRAWER_LINGER + 1.0 {
            self.drawer.open = false;
            ctx.animate_bool_with_time(id, false, 0.0);
        }
        let layout = DrawerLayout::new(edge, row);
        let pointer = ctx.input(|i| i.pointer.hover_pos()).filter(|_| focused);
        let over = pointer.is_some_and(|at| {
            if self.drawer.open {
                layout.near.contains(at)
            } else {
                // wherever the gear is just now, which is still on its way back when it was
                // put away a moment ago
                Rect::from_center_size(
                    layout.gear_at(self.drawer.out),
                    Vec2::splat(2.0 * GEAR_HALF),
                )
                .expand(4.0)
                .contains(at)
            }
        });
        if !gear {
            self.drawer.open = false;
        } else if over {
            self.drawer.open = true;
            self.drawer.near_at = now;
        } else if self.drawer.open {
            let left = self.drawer.near_at + DRAWER_LINGER - now;
            if left <= 0.0 {
                self.drawer.open = false;
            } else {
                ctx.request_repaint_after(Duration::from_secs_f64(left));
            }
        }
        let out = if self.state.effects.transitions() {
            ctx.animate_bool_with_time_and_easing(id, self.drawer.open, 0.24, cubic_out)
        } else {
            f32::from(self.drawer.open)
        };
        self.drawer.out = out;
        let colour = if self.drawer.open || over {
            style::CREAM
        } else {
            style::CREAM_DIM
        };
        if out <= 0.0 {
            if gear {
                style::gear(ui.painter(), layout.gear_at(out), 9.0, colour);
            }
            return;
        }
        let shift = layout.shift(out);
        let acted = egui::Area::new(id.with("area"))
            .order(egui::Order::Foreground)
            .fixed_pos(layout.block.union(layout.tab).min + shift)
            .constrain(false)
            .show(&ctx, |ui| {
                ui.set_opacity(out);
                // the whole strip belongs to the drawer, so clicks on it miss what is beneath
                ui.allocate_rect(layout.block.translate(shift), Sense::hover());
                ui.allocate_rect(layout.tab.translate(shift), Sense::hover());
                style::drawer_shadow(ui.painter(), layout.backing.translate(shift));
                style::drawer_backing(ui.painter(), layout.backing.translate(shift));
                style::drawer_tab(ui.painter(), layout.tab.translate(shift), layout.tab_round);
                style::drawer_outline(
                    ui.painter(),
                    layout.backing.translate(shift),
                    layout.tab.translate(shift),
                    layout.tab_round,
                );
                if gear {
                    // the gear stays fully there while the drawer fades in around it
                    let mut painter = ui.painter().clone();
                    painter.set_opacity(1.0);
                    style::gear(&painter, layout.gear_at(out), 9.0, colour);
                }
                ui.scope_builder(
                    egui::UiBuilder::new().max_rect(layout.content.translate(shift)),
                    |ui| {
                        style::scroll()
                            .auto_shrink(false)
                            .show(ui, |ui| self.drawer_contents(ui, track))
                            .inner
                    },
                )
                .inner
            })
            .inner;
        if acted {
            self.drawer.open = false;
        }
    }

    /// What the drawer holds: a section each for the visualizer and the lyrics. Returns whether
    /// something was chosen that should put it away.
    fn drawer_contents(&mut self, ui: &mut egui::Ui, track: &Track) -> bool {
        drawer_title(ui, "VISUALIZER");
        self.viz_menu(ui, false);
        ui.add_space(16.0);
        drawer_title(ui, "LYRICS");
        let acted = self.lyrics_menu(ui, track);
        acted || self.viz.full
    }

    fn details(&mut self, ui: &mut egui::Ui, track: &Track) {
        ui.add_space(((ui.available_height() - 150.0) / 2.0).max(0.0));
        // the title is there at once; what is under it comes in after it
        let reveal = self
            .reveal
            .over(Rect::from_min_size(ui.cursor().min, vec2(1.0, 100.0)));
        reveal.show(ui, |ui| {
            ui.label(
                RichText::new(&track.title)
                    .font(style::display(36.0))
                    .strong()
                    .color(style::CREAM),
            )
        });
        reveal.show(ui, |ui| {
            ui.label(
                RichText::new(display(&track.artist, "Unknown artist"))
                    .size(22.0)
                    .color(self.accent),
            )
        });
        let album = [Some(track.album.clone()), track.year.map(|y| y.to_string())]
            .into_iter()
            .flatten()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join("  ·  ");
        if !album.is_empty() {
            ui.add_space(6.0);
            reveal.show(ui, |ui| {
                ui.label(RichText::new(album).size(16.0).color(style::CREAM_DIM))
            });
        }
        ui.add_space(10.0);
        reveal.show(ui, |ui| self.star_row(ui, track));
    }

    /// Everything about the lyrics: whether and how they show, and fixing wrong ones (type
    /// them, bring them in from a file, drop them, or go back to the automatic ones). Kept in
    /// the app's data, so the music files stay as they are. Returns whether an action was taken
    /// that should put the drawer away.
    fn lyrics_menu(&mut self, ui: &mut egui::Ui, track: &Track) -> bool {
        let dir = self.data_dir.join("lyrics");
        let mut acted = false;
        ui.checkbox(&mut self.state.visualizer.lyrics, "Lyrics")
            .on_hover_text("Show the lyrics over the visualizer, on its tab and full screen");
        self.lyric_style_menu(ui);
        ui.separator();
        ui.horizontal_wrapped(|ui| {
            // the same first two choices as the Lyrics tab's quick options
            for (quick, label, _) in self
                .quick_items(NowPanel::Lyrics, track)
                .into_iter()
                .filter(|(quick, ..)| {
                    matches!(quick, Quick::LookUp | Quick::TryAgain | Quick::Type)
                })
            {
                let label = match quick {
                    Quick::LookUp => "Look up online".to_owned(),
                    Quick::Type => "Edit lyrics".to_owned(),
                    _ => label,
                };
                if ui.button(label).on_hover_text(quick.hint()).clicked() {
                    self.quick_apply(ui.ctx(), quick, track);
                    acted = true;
                }
            }
            if ui
                .button("Clear lyrics")
                .on_hover_text("Wrong lyrics: forget them and stop looking for this track")
                .clicked()
            {
                self.save_lyrics(&dir, track, "");
                acted = true;
            }
            if ui.button("Import from file...").clicked() {
                self.import_lyrics_file(ui.ctx());
                acted = true;
            }
        });
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(
                    self.lyrics.online_shown(),
                    egui::Button::new("Wrong lyrics, find others"),
                )
                .on_hover_text(
                    "These online lyrics are wrong: never offer them for this track again and \
                     look for different ones. Clear lyrics does the same but then stops looking.",
                )
                .clicked()
            {
                self.wrong_lyrics(&dir, track);
                acted = true;
            }
            if lyrics::has_rejected(&dir, track)
                && ui
                    .button("Allow refused lyrics again")
                    .on_hover_text("Forget which lyrics were called wrong for this track")
                    .clicked()
            {
                lyrics::forget_rejected(&dir, track);
                self.lyrics.ask_again(&dir, track);
                acted = true;
            }
        });
        let online = self.state.lyric_sources();
        ui.horizontal_wrapped(|ui| {
            let button = ui
                .add_enabled(
                    online.allowed
                        && (online.fallback || online.genius)
                        && self.lyrics.other_state != OtherState::Searching,
                    egui::Button::new("Try another source"),
                )
                .on_hover_text(
                    "Ask the other lyric services and switch to different lyrics if any turn up; \
                     otherwise these stay as they are. Needs online lookup and NetEase or Genius switched on.",
                );
            if button.clicked() {
                self.lyrics.try_another(ui.ctx(), track, &dir, online);
            }
            if lyrics::has_user_version(&dir, track)
                && ui
                    .button("Back to automatic lyrics")
                    .on_hover_text("Drop what was typed, imported or cleared and look again")
                    .clicked()
            {
                lyrics::restore(&dir, track);
                self.lyrics.retry();
                acted = true;
            }
        });
        if online.allowed && !self.lyrics.notes.is_empty() {
            egui::CollapsingHeader::new(
                RichText::new("What each source said")
                    .size(12.0)
                    .color(style::CREAM_DIM),
            )
            .id_salt("lyric-notes")
            .show(ui, |ui| {
                for (source, said) in &self.lyrics.notes {
                    ui.label(
                        RichText::new(format!("{source}: {said}"))
                            .size(12.0)
                            .color(style::CREAM_DIM),
                    );
                }
            });
        }
        match &self.lyrics.other_state {
            OtherState::Idle => {}
            OtherState::Searching => {
                ui.label(RichText::new("Looking...").small().color(style::CREAM_DIM));
            }
            OtherState::NothingElse(note) => {
                ui.label(RichText::new(note).small().color(style::CREAM_DIM));
            }
        }
        acted
    }

    /// Every effect on the lyrics panel can be switched off here.
    fn lyric_style_menu(&mut self, ui: &mut egui::Ui) {
        let style = &mut self.state.lyric_style;
        ui.checkbox(&mut style.fade, "Fade lines away from the current one");
        ui.checkbox(&mut style.ad_libs, "Smaller, dimmer (ad-libs)");
        ui.checkbox(&mut style.effects, "Show text effects ({wave|...})");
        ui.checkbox(
            &mut style.effects_live_only,
            "Effects only on the line being sung",
        );
        ui.checkbox(&mut style.animate, "Animate effects");
        ui.checkbox(&mut style.sweep, "Fill the sung line as it goes");
        ui.checkbox(&mut style.shadow, "Soft shadow behind the text");
        ui.checkbox(&mut style.source, "Say where the lyrics came from");
        ui.horizontal_wrapped(|ui| {
            ui.label("Rhymes");
            for (shown, name) in [
                (RhymeShow::Off, "Off"),
                (RhymeShow::Current, "Current line"),
                (RhymeShow::All, "All lines"),
            ] {
                ui.selectable_value(&mut style.rhymes, shown, name);
            }
        });
        if ui.button("Reset display").clicked() {
            *style = Default::default();
        }
    }

    /// Refuses the online lyrics on show for good and looks for different ones.
    fn wrong_lyrics(&mut self, dir: &std::path::Path, track: &Track) {
        self.lyrics.reject_shown(dir, track);
        self.lyrics.ask_again(dir, track);
    }

    fn save_lyrics(&mut self, dir: &std::path::Path, track: &Track, text: &str) {
        // clearing online lyrics also keeps them from coming back with "Automatic"
        if text.is_empty() {
            self.lyrics.reject_shown(dir, track);
        }
        match lyrics::save(dir, track, text) {
            Ok(()) => self.lyrics.retry(),
            Err(error) => self.message(format!("Could not save the lyrics: {error}")),
        }
    }

    fn import_lyrics_file(&mut self, ctx: &egui::Context) {
        let (tx, rx) = std::sync::mpsc::channel();
        self.lyrics.picking = Some(rx);
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let chosen = rfd::FileDialog::new()
                .set_title("Choose a lyrics file")
                .add_filter("Lyrics", &["lrc", "txt"])
                .pick_file();
            // lyrics are small; anything big is not lyrics
            if let Some(text) = chosen
                .filter(|p| std::fs::metadata(p).is_ok_and(|m| m.len() < 1 << 20))
                .and_then(|p| std::fs::read_to_string(p).ok())
            {
                let _ = tx.send(text);
            }
            ctx.request_repaint();
        });
    }

    /// Text box for the lyrics, plain or `[mm:ss.xx]` timed. While the song plays, Stamp puts
    /// the current time on the line under the cursor and moves to the next, to time them by ear.
    fn lyrics_editor(&mut self, ui: &mut egui::Ui, track: &Track, mut text: String, now: f64) {
        let (id, dir) = (egui::Id::new("lyrics-edit"), self.data_dir.join("lyrics"));
        let stamp_key = ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::Enter));
        ui.label(
            RichText::new(
                "One line per lyric. Add [mm:ss.xx] to the start of a line to make it follow the song; effects are written as {wave|words}.",
            )
            .small()
            .color(style::CREAM_DIM),
        );
        // effects wrap the selected words, or the whole line when nothing is selected
        let mut effect: Option<lyricfx::Edit> = None;
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Effect").small().color(style::CREAM_DIM));
            for (name, flag) in lyricfx::NAMES {
                if ui
                    .small_button(name)
                    .on_hover_text("Selected words, or the whole line if nothing is selected. Again to remove.")
                    .clicked()
                {
                    effect = Some(lyricfx::Edit::Toggle(flag));
                }
            }
            if ui.small_button("clear").on_hover_text("Remove effects from the selection or line").clicked() {
                effect = Some(lyricfx::Edit::ClearEffects);
            }
        });
        // hand-set rhymes: the same colour on words that rhyme, whatever the dictionary thinks
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Rhyme").small().color(style::CREAM_DIM));
            for n in 1..=lyricfx::RHYME_COLORS {
                let swatch = egui::Button::new(RichText::new("  ").small())
                    .fill(lyricview::rhyme_color(usize::from(n) - 1))
                    .min_size(vec2(22.0, 16.0));
                if ui
                    .add(swatch)
                    .on_hover_text("Colour the selected words (or the line's last word if nothing is selected) as one rhyme. Again to undo.")
                    .clicked()
                {
                    effect = Some(lyricfx::Edit::Rhyme(n));
                }
            }
            if ui.small_button("never").on_hover_text("These words are never coloured").clicked() {
                effect = Some(lyricfx::Edit::Rhyme(lyricfx::NO_RHYME));
            }
            if ui.small_button("auto").on_hover_text("Back to the automatic rhymes").clicked() {
                effect = Some(lyricfx::Edit::Rhyme(0));
            }
        });
        let room = (ui.available_height() - 96.0).max(80.0);
        style::scroll().max_height(room).show(ui, |ui| {
            ui.add(
                egui::TextEdit::multiline(&mut text)
                    .id(id)
                    .desired_width(f32::INFINITY)
                    .desired_rows(14),
            );
        });
        let (mut save, mut cancel, mut stamp) = (false, false, stamp_key);
        ui.horizontal(|ui| {
            save = ui.button("Save").clicked();
            cancel = ui.button("Cancel").clicked();
            stamp |= ui
                .button("Stamp line")
                .on_hover_text("Ctrl+Enter: put the song's current time on this line")
                .clicked();
            if ui.button("Import from file...").clicked() {
                self.import_lyrics_file(ui.ctx());
            }
        });
        if stamp {
            let mut state = egui::TextEdit::load_state(ui.ctx(), id).unwrap_or_default();
            let at = state
                .cursor
                .char_range()
                .map_or(text.chars().count(), |r| r.primary.index);
            let (stamped, next) = lyrics::stamp_line(&text, at, now);
            text = stamped;
            state
                .cursor
                .set_char_range(Some(CCursorRange::one(CCursor::new(next))));
            state.store(ui.ctx(), id);
            ui.memory_mut(|m| m.request_focus(id));
        }
        if let Some(effect) = effect {
            let mut state = egui::TextEdit::load_state(ui.ctx(), id).unwrap_or_default();
            let (a, b) = state
                .cursor
                .char_range()
                .map_or((0, 0), |r| (r.primary.index, r.secondary.index));
            text = lyricfx::apply(&text, a, b, effect);
            let end = text.chars().count();
            state.cursor.set_char_range(Some(CCursorRange::two(
                CCursor::new(a.min(end)),
                CCursor::new(b.min(end)),
            )));
            state.store(ui.ctx(), id);
            ui.memory_mut(|m| m.request_focus(id));
        }
        if save {
            self.save_lyrics(&dir, track, &text);
        } else if !cancel {
            self.lyrics.editing = Some(text);
        }
    }

    /// Synced lyrics follow the song (click a line to jump there); plain ones just scroll.
    fn lyrics(&mut self, ui: &mut egui::Ui, track: &skratch::model::Track) {
        let dir = self.data_dir.join("lyrics");
        self.lyrics
            .update(ui.ctx(), track, &dir, self.state.lyric_sources());
        let now = if self.playing {
            self.reactive.now()
        } else {
            self.state.position
        };
        if let Some(rx) = &self.lyrics.picking {
            match rx.try_recv() {
                Ok(text) => {
                    self.lyrics.picking = None;
                    self.lyrics.editing = Some(text);
                }
                // the dialog was cancelled
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.lyrics.picking = None,
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
        if let Some(text) = self.lyrics.editing.take() {
            self.lyrics_editor(ui, track, text, now);
            return;
        }
        let height = ui.available_height();
        let shown = self.state.lyric_style.clone();
        let reveal = self.reveal;
        // a line under the lyrics says which service they came from
        let origin = self
            .lyrics
            .origin
            .filter(|_| shown.source && matches!(self.lyrics.status, Status::Found(_)));
        let scroll_height = if origin.is_some() {
            (height - 20.0).max(0.0)
        } else {
            height
        };
        let look = lyricview::Look {
            style: &shown,
            time: ui.input(|i| i.time),
        };
        if self.lyrics.marks.is_none()
            && shown.rhymes != RhymeShow::Off
            && let Status::Found(found) = &self.lyrics.status
        {
            let texts: Vec<String> = match found {
                Lyrics::Plain(text) => text.lines().map(String::from).collect(),
                Lyrics::Synced(lines) => lines.iter().map(|(_, t)| t.clone()).collect(),
            };
            self.lyrics.marks = Some(lyricview::rhyme_marks(&texts));
        }
        let dim = |text: String| RichText::new(text).size(18.0).color(style::CREAM_DIM);
        match &self.lyrics.status {
            Status::Loading => {
                ui.add_space(8.0);
                ui.label(dim("Looking for lyrics...".into()));
            }
            Status::Found(Lyrics::Plain(text)) => {
                let marks = self.lyrics.marks.as_deref().unwrap_or(&[]);
                style::scroll()
                    .id_salt("lyrics")
                    .max_height(scroll_height)
                    .auto_shrink(false)
                    .show(ui, |ui| {
                        for (i, line) in text.lines().enumerate() {
                            let found = marks.get(i).map_or(&[][..], Vec::as_slice);
                            let found = if shown.rhymes == RhymeShow::Off {
                                &[]
                            } else {
                                found
                            };
                            let spec = lyricview::Spec {
                                size: 18.0,
                                scale: 1.0,
                                color: style::CREAM,
                                fade: 1.0,
                                effects: true,
                                rhyme: if shown.rhymes == RhymeShow::Off {
                                    0.0
                                } else {
                                    1.0
                                },
                                sweep: None,
                            };
                            reveal
                                .show(ui, |ui| lyricview::draw_line(ui, line, found, &spec, &look));
                        }
                    });
            }
            Status::Found(Lyrics::Synced(lines)) => {
                let current = lines.partition_point(|(t, _)| *t <= now).checked_sub(1);
                let marks = self.lyrics.marks.as_deref().unwrap_or(&[]);
                style::scroll()
                    .id_salt("lyrics")
                    .max_height(scroll_height)
                    .auto_shrink(false)
                    .show(ui, |ui| {
                        ui.add_space(height / 3.0);
                        for (i, (time, text)) in lines.iter().enumerate() {
                            if text.is_empty() {
                                ui.add_space(14.0);
                                continue;
                            }
                            let here = Some(i) == current;
                            // every line is laid out the same; the sung one is drawn bigger
                            let away = current.map_or(1, |c| i.abs_diff(c)) as f32;
                            let id = egui::Id::new(("lyric-line", i));
                            let ctx = ui.ctx();
                            let live = if here { lyricview::LIVE_SCALE } else { 1.0 };
                            let scale = ctx.animate_value_with_time(id.with(0), live, 0.25);
                            let fade = if here {
                                1.0
                            } else if shown.fade {
                                (0.95 - 0.1 * away).max(0.3)
                            } else {
                                0.8
                            };
                            let fade = ctx.animate_value_with_time(id.with(1), fade, 0.25);
                            // rhyme colours fade out with distance from the sung line
                            let reach = if here {
                                1.0
                            } else {
                                (1.0 - away / 5.0).max(0.0)
                            };
                            let rhyme = ctx.animate_value_with_time(id.with(2), reach, 0.25);
                            let found = marks.get(i).map_or(&[][..], Vec::as_slice);
                            let show = match shown.rhymes {
                                RhymeShow::All => true,
                                RhymeShow::Current => here,
                                RhymeShow::Off => false,
                            };
                            let found = if show { found } else { &[] };
                            // the sung line fills with the accent colour as its time runs out
                            let sweep = (here && shown.sweep && self.playing).then(|| {
                                let words = lyricview::line_syllables(text);
                                let next = lines.get(i + 1).map_or(*time + 5.0, |l| l.0);
                                let span = (next - time).clamp(0.5, 0.45 * f64::from(words) + 0.6);
                                (((now - time) / span).clamp(0.0, 1.0) as f32, self.accent)
                            });
                            let spec = lyricview::Spec {
                                size: 20.0,
                                scale,
                                color: if sweep.is_some() {
                                    style::CREAM
                                } else if here {
                                    self.accent
                                } else {
                                    style::CREAM_DIM
                                },
                                fade,
                                effects: here || !shown.effects_live_only,
                                rhyme: if show { rhyme } else { 0.0 },
                                sweep,
                            };
                            let line = reveal
                                .show(ui, |ui| lyricview::draw_line(ui, text, found, &spec, &look))
                                .on_hover_cursor(egui::CursorIcon::PointingHand);
                            if line.clicked() {
                                self.action = Some(Action::Seek(*time));
                            }
                            if here && self.lyrics.scrolled != current {
                                line.scroll_to_me(Some(Align::Center));
                            }
                        }
                        ui.add_space(height / 3.0);
                    });
                self.lyrics.scrolled = current;
            }
            Status::Missing => {
                ui.add_space(8.0);
                ui.label(dim("No lyrics for this track".into()));
            }
        }
        if let Some(origin) = origin {
            reveal.show(ui, |ui| {
                ui.with_layout(egui::Layout::right_to_left(Align::Min), |ui| {
                    let text = RichText::new(format!("from {}", origin.label()))
                        .size(11.0)
                        .color(style::CREAM_DIM);
                    // a service's name is a link to its page for this song
                    match origin.page(&track.artist, &track.title) {
                        Some(url) => {
                            let link = ui
                                .add(egui::Label::new(text).sense(egui::Sense::click()))
                                .on_hover_cursor(egui::CursorIcon::PointingHand)
                                .on_hover_text(format!("Open {url}"));
                            if link.hovered() {
                                ui.painter().hline(
                                    link.rect.x_range(),
                                    link.rect.bottom() - 1.0,
                                    egui::Stroke::new(1.0_f32, style::CREAM_DIM),
                                );
                            }
                            if link.clicked() {
                                ui.ctx().open_url(egui::OpenUrl::new_tab(url));
                            }
                        }
                        None => {
                            ui.label(text);
                        }
                    }
                });
            });
        }
    }

    /// The queue or the whole library as clickable rows, scrolled to the playing track, under
    /// a search bar that is the same on both tabs and stays put when switching between them.
    fn panel_list(&mut self, ui: &mut egui::Ui, panel: NowPanel) {
        let up_next = panel == NowPanel::UpNext;
        let reveal = self.reveal;
        // Library search stays within what is playing when that is an album or playlist
        let scoped = !up_next && matches!(self.state.source, Source::Tracks(_));
        let chip = scoped.then_some(("All tracks", "Search the whole library instead"));
        let id = egui::Id::new(super::SEARCH_ID);
        let (typed, widen) =
            style::search_field(ui, &mut self.search, id, "Search", chip, self.accent);
        if widen {
            self.state.source = Source::Library;
        }
        if typed.changed() {
            self.rebuild();
        }
        ui.add_space(6.0);
        // (track index, position in the queue, position among the visible tracks)
        let rows: Vec<(usize, Option<usize>, usize)> = if up_next {
            let query = self.search.to_lowercase();
            self.state
                .queue
                .ids
                .iter()
                .enumerate()
                .filter_map(|(q, id)| self.by_id.get(id).map(|i| (*i, Some(q), 0)))
                .filter(|(i, ..)| matches_query(&self.tracks[*i], &query))
                .collect()
        } else {
            match &self.state.source {
                Source::Library => self
                    .filtered
                    .iter()
                    .enumerate()
                    .map(|(r, i)| (*i, None, r))
                    .collect(),
                Source::Tracks(ids) => {
                    let matching: std::collections::HashSet<usize> =
                        self.filtered.iter().copied().collect();
                    ids.iter()
                        .filter_map(|id| self.by_id.get(id))
                        .filter(|i| matching.contains(i))
                        .enumerate()
                        .map(|(r, i)| (*i, None, r))
                        .collect()
                }
            }
        };
        self.list_ids = rows.iter().map(|(i, ..)| self.tracks[*i].id).collect();
        if rows.is_empty() {
            let nothing = match (!self.search.is_empty(), up_next) {
                (true, _) => "Nothing matches.",
                (false, true) => "The queue is empty.",
                (false, false) => "No tracks yet.",
            };
            ui.add_space(8.0);
            ui.label(RichText::new(nothing).color(style::CREAM_DIM));
        }
        let current = self.state.queue.current_id();
        let now = ui.input(|i| i.time);
        let bounds = ui.available_rect_before_wrap();
        let browsing = ui.rect_contains_pointer(bounds);
        let mut area = style::scroll()
            .id_salt(("now-list", panel as u8))
            .auto_shrink(false);
        if self.listed != current {
            // Follow the playing track, except when the user is looking through the list or
            // just clicked a row here (that row is already in view). `None` means the panel
            // was just opened, which always scrolls.
            let stay = self.listed.is_some() && (browsing || now < self.list_hold);
            self.listed = current;
            let at = rows
                .iter()
                .position(|(i, ..)| Some(self.tracks[*i].id) == current);
            if let (false, Some(at)) = (stay, at) {
                let stride = 40.0 + ui.spacing().item_spacing.y;
                let centred = at as f32 * stride + stride / 2.0 - bounds.height() / 2.0;
                area = area.vertical_scroll_offset(centred.max(0.0));
            }
        }
        area.show_rows(ui, 40.0, rows.len(), |ui, range| {
            for r in range {
                let (index, queue, context) = rows[r];
                reveal.show(ui, |ui| self.track_row(ui, index, queue, context));
            }
        });
        if browsing && ui.input(|i| i.pointer.any_click()) {
            self.list_hold = now + 1.0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reveal(progress: f32) -> Reveal {
        Reveal {
            progress,
            top: 100.0,
            span: 400.0,
        }
    }

    #[test]
    fn the_tab_animation_leaves_the_top_alone_and_brings_the_bottom_in_last() {
        let ys: Vec<f32> = (0..=8).map(|k| 100.0 + 50.0 * k as f32).collect();
        // the top is fully there from the first frame, however far along it is
        for step in 0..=10 {
            assert_eq!(reveal(step as f32 / 10.0).at(100.0), 1.0);
        }
        // at the start the bottom is not there at all, and the middle is half there
        assert_eq!(reveal(0.0).at(500.0), 0.0);
        assert!((reveal(0.0).at(300.0) - 0.5).abs() < 1e-6);
        for step in 0..=20 {
            let now = reveal(step as f32 / 20.0);
            // lower parts are never more visible than the ones above them...
            let column: Vec<f32> = ys.iter().map(|y| now.at(*y)).collect();
            assert!(
                column.windows(2).all(|pair| pair[0] >= pair[1] - 1e-6),
                "{column:?}"
            );
            // ...and everything only ever becomes more visible
            if step > 0 {
                let before = reveal((step - 1) as f32 / 20.0);
                assert!(ys.iter().all(|y| now.at(*y) >= before.at(*y) - 1e-6));
            }
        }
        // finished means everything shows; places outside the area count as its ends
        assert_eq!(reveal(1.0).at(500.0), 1.0);
        assert_eq!(Reveal::DONE.at(9999.0), 1.0);
        assert_eq!(reveal(0.0).at(-50.0), 1.0);
        assert_eq!(reveal(0.0).at(10_000.0), 0.0);
        assert_eq!(Reveal::DONE.uniform(), 1.0);
    }

    fn row(tabs_right: f32, right: f32) -> GearRow {
        GearRow {
            tabs: Rect::from_min_max(pos2(900.0, 60.0), pos2(tabs_right, 88.0)),
            right,
        }
    }

    #[test]
    fn the_gear_rides_the_drawer_left_without_crossing_the_tabs() {
        // a wide page: the gear starts at the end of the tab row and ends over the options'
        // left edge, on the same row
        let edge = Rect::from_min_max(pos2(220.0, 50.0), pos2(1900.0, 880.0));
        let wide = DrawerLayout::new(edge, row(1220.0, 1876.0));
        assert_eq!(wide.gear_in, pos2(1862.0, 74.0));
        assert_eq!(wide.gear_out.y, wide.gear_in.y);
        assert!(wide.gear_out.x > 1220.0 && wide.gear_out.x < wide.gear_in.x);
        // the gear sits in a tab that hangs off the drawer's left edge, on the tab row
        assert_eq!(wide.tab.right(), wide.backing.left());
        assert_eq!(wide.gear_out, wide.tab.center());
        assert!(wide.tab.left() > 1220.0);
        // the drawer runs the full height of the page, the tab hangs off it on the tab row
        assert_eq!(wide.backing.top(), edge.top());
        assert!(wide.tab.top() > edge.top() && wide.tab.bottom() <= 88.0 + GEAR_HALF);
        // the drawer slides exactly as far as the gear has to go
        assert_eq!(wide.shift(0.0).x, wide.gear_in.x - wide.gear_out.x);
        assert_eq!(wide.shift(1.0), Vec2::ZERO);
        assert_eq!(wide.gear_at(0.0), wide.gear_in);
        assert_eq!(wide.gear_at(1.0), wide.gear_out);
        // the pointer stays on the open drawer at both ends of the gear's trip and under it
        assert!(wide.near.contains(wide.gear_in));
        assert!(wide.near.contains(wide.gear_out));
        assert!(wide.near.contains(wide.content.center()));
        // the options sit inside the backing, below the tab row
        assert!(wide.backing.contains_rect(wide.content));
        assert!(wide.content.top() > 88.0);
        // a narrow page: the tab still fits beside the drawer, inside the page
        let edge = Rect::from_min_max(pos2(220.0, 50.0), pos2(1000.0, 880.0));
        let narrow = DrawerLayout::new(edge, row(950.0, 976.0));
        assert!(narrow.tab.left() >= edge.left());
        assert!(narrow.gear_out.x < narrow.gear_in.x);
    }

    #[test]
    fn the_pull_tab_fits_in_a_narrow_panel() {
        // a 464 px panel: the drawer takes 300 and the tab hangs off its left edge
        let edge = Rect::from_min_max(pos2(0.0, 0.0), pos2(464.0, 680.0));
        let row = GearRow {
            tabs: Rect::from_min_max(pos2(20.0, 8.0), pos2(333.0, 36.0)),
            right: 464.0,
        };
        let layout = DrawerLayout::new(edge, row);
        assert_eq!(layout.tab.right(), layout.backing.left());
        assert!(layout.tab.left() >= 0.0);
        assert!(layout.gear_out.x < layout.gear_in.x);
        // even a panel hardly wider than the tab keeps the tab inside it
        let tiny = DrawerLayout::new(Rect::from_min_max(pos2(0.0, 0.0), pos2(200.0, 400.0)), row);
        assert!(tiny.tab.left() >= 0.0);
    }

    #[test]
    fn up_next_and_library_are_the_lists() {
        assert!(is_list(NowPanel::UpNext) && is_list(NowPanel::Library));
        assert!(!is_list(NowPanel::Details) && !is_list(NowPanel::Lyrics));
        assert!(!is_list(NowPanel::Visualizer));
    }

    #[test]
    fn lyrics_and_visualizer_are_one_page() {
        assert!(same_page(NowPanel::Lyrics, NowPanel::Visualizer));
        assert!(same_page(NowPanel::UpNext, NowPanel::Library));
        assert!(!same_page(NowPanel::Lyrics, NowPanel::Library));
        assert!(!same_page(NowPanel::Details, NowPanel::Lyrics));
    }
}
