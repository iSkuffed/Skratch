//! The spectrum visualizer: a panel on the Now playing page, and a full-screen view with or
//! without the record. The spectrum is analysed ahead of playback (see `analysis`), so drawing
//! only reads numbers and smooths them into bars that jump up and fall back.
use super::{Skratch, style};
use crate::state::{LyricSide, Visualizer, VizStyle};
use eframe::egui::{
    self, Align2, Color32, FontId, Mesh, Painter, Pos2, Rect, RichText, Stroke, pos2, vec2,
};
use skratch::{
    analysis::BANDS,
    lyrics::{Lyrics, Status},
};

/// Smoothed bars and falling caps, kept between frames.
pub struct Viz {
    levels: [f32; BANDS],
    caps: [f32; BANDS],
    fall: [f32; BANDS],
    /// The full-screen view is open.
    pub full: bool,
    /// The window itself is full screen too.
    os_full: bool,
    /// When the pointer last moved in the full-screen view, for hiding the controls.
    moved_at: f64,
}

impl Default for Viz {
    fn default() -> Self {
        Self {
            levels: [0.0; BANDS],
            caps: [0.0; BANDS],
            fall: [0.0; BANDS],
            full: false,
            os_full: false,
            moved_at: 0.0,
        }
    }
}

impl Viz {
    /// Moves the bars towards `target`: up at once, down like gravity.
    fn step(&mut self, target: Option<[f32; BANDS]>, dt: f32) -> bool {
        let mut moving = false;
        for i in 0..BANDS {
            let goal = target.map_or(0.0, |t| t[i]);
            let level = &mut self.levels[i];
            *level = if goal > *level {
                *level + (goal - *level) * (1.0 - (-30.0 * dt).exp())
            } else {
                (*level - 1.6 * dt).max(goal)
            };
            // the cap rides the bar up, hangs for a moment, then falls faster and faster
            if *level >= self.caps[i] {
                self.caps[i] = *level;
                self.fall[i] = -0.25;
            } else {
                self.fall[i] += 2.2 * dt;
                self.caps[i] = (self.caps[i] - self.fall[i].max(0.0) * dt).max(*level);
            }
            moving |= *level > 0.003 || self.caps[i] > 0.003;
        }
        moving
    }

    /// Draws the bars into `rect`. `strength` fades the whole thing (under lyrics).
    #[allow(clippy::too_many_arguments)]
    fn paint(
        &self,
        painter: &Painter,
        rect: Rect,
        cfg: &Visualizer,
        accent: Color32,
        beat: f32,
        strength: f32,
        hole: Option<f32>,
    ) {
        let hot = accent.lerp_to_gamma(style::CREAM, 0.55);
        let base = accent.gamma_multiply(0.55);
        let tint = |c: Color32, a: f32| c.gamma_multiply(a * strength);
        // a soft glow in the middle that swells with the bass
        let centre = rect.center();
        let size = rect.width().min(rect.height());
        for (grow, alpha) in [(0.55, 0.03), (0.4, 0.04), (0.27, 0.05)] {
            painter.circle_filled(centre, size * (grow + 0.06 * beat), tint(accent, alpha));
        }
        let gradient = |mesh: &mut Mesh, quad: [Pos2; 4], colours: [Color32; 4]| {
            let at = mesh.vertices.len() as u32;
            for (pos, color) in quad.into_iter().zip(colours) {
                mesh.vertices.push(egui::epaint::Vertex {
                    pos,
                    uv: egui::epaint::WHITE_UV,
                    color,
                });
            }
            mesh.indices
                .extend([at, at + 1, at + 2, at, at + 2, at + 3]);
        };
        let mut mesh = Mesh::default();
        match cfg.style {
            VizStyle::Bars => {
                let floor = if cfg.reflection {
                    rect.bottom() - rect.height() * 0.2
                } else {
                    rect.bottom() - 6.0
                };
                let height = floor - rect.top() - 10.0;
                let slot = rect.width() / BANDS as f32;
                let width = (slot * 0.72).max(2.0);
                for i in 0..BANDS {
                    let x = rect.left() + slot * (i as f32 + 0.5) - width / 2.0;
                    let top = floor - self.levels[i] * height;
                    let mid = |t: f32| base.lerp_to_gamma(hot, t);
                    // brighter towards the top of the bar
                    gradient(
                        &mut mesh,
                        [
                            pos2(x, top),
                            pos2(x + width, top),
                            pos2(x + width, floor),
                            pos2(x, floor),
                        ],
                        [
                            tint(mid(self.levels[i]), 1.0),
                            tint(mid(self.levels[i]), 1.0),
                            tint(base, 0.9),
                            tint(base, 0.9),
                        ],
                    );
                    if cfg.reflection {
                        let depth =
                            (self.levels[i] * height * 0.45).min(rect.bottom() - floor - 4.0);
                        gradient(
                            &mut mesh,
                            [
                                pos2(x, floor + 3.0),
                                pos2(x + width, floor + 3.0),
                                pos2(x + width, floor + 3.0 + depth),
                                pos2(x, floor + 3.0 + depth),
                            ],
                            [
                                tint(base, 0.28),
                                tint(base, 0.28),
                                tint(base, 0.0),
                                tint(base, 0.0),
                            ],
                        );
                    }
                    if cfg.peak_caps && self.caps[i] > 0.01 {
                        let y = floor - self.caps[i] * height - 4.0;
                        gradient(
                            &mut mesh,
                            [
                                pos2(x, y - 2.0),
                                pos2(x + width, y - 2.0),
                                pos2(x + width, y + 1.0),
                                pos2(x, y + 1.0),
                            ],
                            [tint(style::CREAM, 0.9); 4],
                        );
                    }
                }
            }
            VizStyle::Mirror => {
                let mid = centre.y;
                let height = rect.height() * 0.46;
                let slot = rect.width() / BANDS as f32;
                let width = (slot * 0.6).max(2.0);
                for i in 0..BANDS {
                    let x = rect.left() + slot * (i as f32 + 0.5) - width / 2.0;
                    let reach = self.levels[i] * height + 1.5;
                    let colour = base.lerp_to_gamma(hot, self.levels[i]);
                    gradient(
                        &mut mesh,
                        [
                            pos2(x, mid - reach),
                            pos2(x + width, mid - reach),
                            pos2(x + width, mid + reach),
                            pos2(x, mid + reach),
                        ],
                        [
                            tint(colour, 1.0),
                            tint(colour, 1.0),
                            tint(colour, 1.0),
                            tint(colour, 1.0),
                        ],
                    );
                    if cfg.peak_caps && self.caps[i] > 0.01 {
                        let y = self.caps[i] * height + 5.0;
                        for sign in [-1.0, 1.0] {
                            let y = mid + sign * y;
                            gradient(
                                &mut mesh,
                                [
                                    pos2(x, y - 1.5),
                                    pos2(x + width, y - 1.5),
                                    pos2(x + width, y + 1.5),
                                    pos2(x, y + 1.5),
                                ],
                                [tint(style::CREAM, 0.8); 4],
                            );
                        }
                    }
                }
            }
            VizStyle::Ring => {
                // the spectrum runs down both sides, so the ring is symmetric: bass at the top
                let inner = hole.unwrap_or(size * 0.2 * (1.0 + 0.04 * beat));
                let reach = (size * 0.5 - inner - 8.0).max(10.0);
                // a thin rim where the bars start, so they sit on the record's edge
                painter.circle_stroke(centre, inner + 1.0, Stroke::new(1.5_f32, tint(base, 0.7)));
                let count = BANDS * 2;
                let stroke = (std::f32::consts::TAU * inner / count as f32 * 0.62).max(1.5);
                for k in 0..count {
                    let i = if k < BANDS { k } else { count - 1 - k };
                    let angle = -std::f32::consts::FRAC_PI_2
                        + k as f32 / count as f32 * std::f32::consts::TAU;
                    let dir = vec2(angle.cos(), angle.sin());
                    let level = self.levels[i];
                    let colour = base.lerp_to_gamma(hot, level);
                    painter.line_segment(
                        [
                            centre + dir * (inner + 4.0),
                            centre + dir * (inner + 4.0 + level * reach + 1.5),
                        ],
                        Stroke::new(stroke, tint(colour, 1.0)),
                    );
                    if cfg.peak_caps && self.caps[i] > 0.01 {
                        painter.circle_filled(
                            centre + dir * (inner + 9.0 + self.caps[i] * reach),
                            stroke * 0.55,
                            tint(style::CREAM, 0.85),
                        );
                    }
                }
            }
        }
        painter.add(egui::Shape::mesh(mesh));
    }
}

impl Skratch {
    /// Smooths this frame's spectrum into the bars and keeps repainting while they move.
    fn viz_step(&mut self, ctx: &egui::Context) {
        let dt = ctx.input(|i| i.stable_dt).min(0.1);
        let target = if self.playing {
            self.reactive.spectrum()
        } else {
            None
        };
        if self.viz.step(target, dt) || self.playing {
            ctx.request_repaint();
        }
    }

    /// The visualizer filling `rect`, for the page panel. `strength` dims it behind lyrics.
    pub(super) fn viz_panel(&mut self, ui: &egui::Ui, rect: Rect, strength: f32) {
        self.viz_step(ui.ctx());
        let painter = ui.painter().with_clip_rect(rect);
        let cfg = self.state.visualizer.clone();
        self.viz
            .paint(&painter, rect, &cfg, self.accent, self.beat, strength, None);
    }

    /// The ring style, drawn around the record at `centre` (radius `r`) on the Now playing page.
    pub(super) fn viz_ring_at(&mut self, ui: &egui::Ui, centre: Pos2, r: f32) {
        self.viz_step(ui.ctx());
        let cfg = self.state.visualizer.clone();
        let inner = r * 1.14;
        let rect = Rect::from_center_size(centre, vec2(1.0, 1.0) * 2.0 * (inner + r * 0.5 + 8.0));
        // it fades in with the view it belongs to, like the rest of the panel
        let mut painter = ui.painter().clone();
        painter.multiply_opacity(self.tab_fade);
        self.viz.paint(
            &painter,
            rect,
            &cfg,
            self.accent,
            self.beat,
            1.0,
            Some(inner),
        );
    }

    /// Options shared by the panel's menu and the full-screen controls.
    pub(super) fn viz_menu(&mut self, ui: &mut egui::Ui, full: bool) {
        let cfg = &mut self.state.visualizer;
        ui.horizontal(|ui| {
            for choice in VizStyle::ALL {
                ui.selectable_value(&mut cfg.style, choice, choice.title());
            }
        });
        ui.checkbox(
            &mut cfg.lyrics,
            if full { "Lyrics" } else { "Lyrics over it" },
        );
        if full {
            // the panel's drawer has this among the lyrics' own display options
            ui.checkbox(
                &mut self.state.lyric_style.shadow,
                "Soft shadow behind lyrics",
            );
            ui.add_enabled_ui(cfg.lyrics, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Lyrics at");
                    for side in LyricSide::ALL {
                        ui.selectable_value(&mut cfg.lyric_side, side, side.title());
                    }
                });
            });
            ui.checkbox(&mut cfg.record, "Spinning record");
        } else {
            ui.checkbox(
                &mut cfg.fill_when_no_lyrics,
                "Fill the lyrics panel when a track has none",
            );
            ui.checkbox(&mut cfg.behind_lists, "Show behind Up next and Library");
        }
        ui.checkbox(&mut cfg.peak_caps, "Falling peak caps");
        ui.checkbox(&mut cfg.reflection, "Reflection (bars)");
        if ui.button("Reset visualizer").clicked() {
            *cfg = Default::default();
        }
    }

    /// The full-screen view: replaces the whole window until Esc.
    pub(super) fn viz_screen(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let rect = ui.max_rect();
        let now = ctx.input(|i| i.time);
        if ctx.input(|i| i.pointer.delta() != egui::Vec2::ZERO || i.pointer.any_down()) {
            self.viz.moved_at = now;
        }
        let controls = now - self.viz.moved_at < 2.5;
        if controls {
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
        } else {
            ctx.set_cursor_icon(egui::CursorIcon::None);
        }
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            self.leave_viz(&ctx);
            return;
        }
        let cfg = self.state.visualizer.clone();
        self.viz_step(&ctx);
        let painter = ui.painter().clone();
        // the lyrics get their own strip, and the visuals keep to the rest of the screen
        let lyric_rect = cfg
            .lyrics
            .then(|| self.viz_lyric_rect(rect, cfg.lyric_side));
        let lyric_rect = lyric_rect.filter(|_| self.viz_has_lyrics(&ctx));
        let scene = match (lyric_rect, cfg.lyric_side) {
            (None, _) => rect,
            (Some(zone), LyricSide::Top) => rect.with_min_y(zone.bottom()),
            (Some(zone), LyricSide::Left) => rect.with_min_x(zone.right()),
            (Some(zone), LyricSide::Right) => rect.with_max_x(zone.left()),
        };
        let size = scene.width().min(scene.height());
        let (centre, radius) = (scene.center(), size * 0.17);
        let with_record = cfg.record && self.current().is_some();
        let pulse = radius * (1.0 + 0.03 * self.beat);
        let hole = (cfg.style == VizStyle::Ring && with_record).then_some(pulse * 1.04);
        self.viz
            .paint(&painter, scene, &cfg, self.accent, self.beat, 1.0, hole);
        if with_record && let Some(track) = self.current().cloned() {
            let art = track
                .artwork
                .as_deref()
                .and_then(|path| self.art.texture(path));
            let label = style::seed_color(&format!("{}{}", track.artist, track.album));
            style::record(&painter, centre, pulse, art, label, self.spin);
        }
        if let Some(zone) = lyric_rect {
            self.viz_lyrics(&painter, zone, cfg.lyric_side);
        }
        if let Some(track) = self.current() {
            painter.text(
                pos2(rect.left() + 24.0, rect.bottom() - 22.0),
                Align2::LEFT_BOTTOM,
                format!(
                    "{}  -  {}",
                    track.title,
                    super::display(&track.artist, "Unknown artist")
                ),
                FontId::proportional(15.0),
                style::CREAM_DIM.gamma_multiply(0.8),
            );
        }
        // double-click toggles the window itself between full screen and normal
        let area = ui.interact(rect, ui.id().with("viz-area"), egui::Sense::click());
        if area.double_clicked() {
            self.viz.os_full = !self.viz.os_full;
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(self.viz.os_full));
        } else if area.clicked() {
            self.perform(super::Action::Pause);
        }
        if controls {
            egui::Area::new(egui::Id::new("viz-controls"))
                .fixed_pos(pos2(rect.right() - 16.0, rect.top() + 14.0))
                .pivot(Align2::RIGHT_TOP)
                .order(egui::Order::Foreground)
                .show(&ctx, |ui| {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        self.viz_menu(ui, true);
                        ui.separator();
                        ui.horizontal(|ui| {
                            let label = if self.viz.os_full {
                                "Leave window full screen"
                            } else {
                                "Window full screen"
                            };
                            if ui.button(label).clicked() {
                                self.viz.os_full = !self.viz.os_full;
                                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(
                                    self.viz.os_full,
                                ));
                            }
                            if ui.button("Close (Esc)").clicked() {
                                self.leave_viz(&ctx);
                            }
                        });
                    });
                });
        }
    }

    fn leave_viz(&mut self, ctx: &egui::Context) {
        self.viz.full = false;
        if self.viz.os_full {
            self.viz.os_full = false;
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
        }
    }

    /// The strip of the screen the lyrics use.
    fn viz_lyric_rect(&self, rect: Rect, side: LyricSide) -> Rect {
        match side {
            LyricSide::Top => rect.with_max_y(rect.top() + (rect.height() * 0.22).max(120.0)),
            LyricSide::Left => rect.with_max_x(rect.left() + rect.width() * 0.32),
            LyricSide::Right => rect.with_min_x(rect.right() - rect.width() * 0.32),
        }
    }

    /// Whether the current track has timed lyrics to show.
    fn viz_has_lyrics(&mut self, ctx: &egui::Context) -> bool {
        let dir = self.data_dir.join("lyrics");
        let Some(track) = self.current().cloned() else {
            return false;
        };
        self.lyrics
            .update(ctx, &track, &dir, self.state.lyric_sources());
        matches!(self.lyrics.status, Status::Found(Lyrics::Synced(_)))
    }

    /// The line being sung, large, with its neighbours dimmer, inside `zone`.
    fn viz_lyrics(&mut self, painter: &Painter, zone: Rect, side: LyricSide) {
        let Status::Found(Lyrics::Synced(lines)) = &self.lyrics.status else {
            return;
        };
        let now = self.reactive.now();
        let Some(current) = lines.partition_point(|(t, _)| *t <= now).checked_sub(1) else {
            return;
        };
        let sideways = side != LyricSide::Top;
        let big = if sideways {
            (zone.width() * 0.085).clamp(18.0, 38.0)
        } else {
            (zone.height() * 0.3).clamp(20.0, 42.0)
        };
        let reach: isize = if sideways { 2 } else { 1 };
        let margin = 36.0;
        let wrap = zone.width() - 2.0 * margin;
        let mut rows = Vec::new();
        for offset in -reach..=reach {
            let Some((_, text)) = current
                .checked_add_signed(offset)
                .and_then(|i| lines.get(i))
                .filter(|l| !l.1.is_empty())
            else {
                continue;
            };
            let (alpha, scale) = match offset {
                0 => (1.0, 1.0),
                1 | -1 => (0.55, 0.72),
                _ => (0.3, 0.6),
            };
            let colour = if offset == 0 {
                self.accent
            } else {
                style::CREAM
            };
            let galley = painter.layout(
                text.clone(),
                FontId::proportional(big * scale),
                colour.gamma_multiply(alpha),
                wrap,
            );
            rows.push((galley, colour));
        }
        let gap = 10.0;
        let height: f32 = rows.iter().map(|(g, _)| g.size().y + gap).sum();
        let mut y = zone.center().y - height / 2.0;
        for (galley, colour) in rows {
            let x = if sideways {
                zone.left() + margin
            } else {
                zone.center().x - galley.size().x / 2.0
            };
            let next = y + galley.size().y + gap;
            painter.add(egui::epaint::TextShape::new(pos2(x, y), galley, colour));
            y = next;
        }
    }

    /// A small button that opens the full-screen view.
    pub(super) fn viz_full_button(&mut self, ui: &mut egui::Ui) {
        if ui
            .small_button(RichText::new("Full screen").color(style::CREAM_DIM))
            .on_hover_text("Fill the window with the visualizer. Esc leaves.")
            .clicked()
        {
            self.viz.full = true;
            self.viz.moved_at = ui.input(|i| i.time);
        }
    }
}
