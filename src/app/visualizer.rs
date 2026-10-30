//! The spectrum visualizer: a panel on the Now playing page, and a full-screen view with or
//! without the record. The spectrum is analysed ahead of playback (see `analysis`), so drawing
//! only reads numbers and smooths them into bars that jump up and fall back.
use super::{Skratch, style};
use crate::state::{Visualizer, VizStyle};
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
                let inner = hole.unwrap_or(size * 0.2) * (1.0 + 0.04 * beat);
                let reach = (size * 0.5 - inner - 8.0).max(10.0);
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
                            centre + dir * (inner + 3.0),
                            centre + dir * (inner + 3.0 + level * reach + 1.5),
                        ],
                        Stroke::new(stroke, tint(colour, 1.0)),
                    );
                    if cfg.peak_caps && self.caps[i] > 0.01 {
                        painter.circle_filled(
                            centre + dir * (inner + 8.0 + self.caps[i] * reach),
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

    /// Options shared by the panel's menu and the full-screen controls.
    pub(super) fn viz_menu(&mut self, ui: &mut egui::Ui, full: bool) {
        let cfg = &mut self.state.visualizer;
        ui.horizontal(|ui| {
            for choice in VizStyle::ALL {
                ui.selectable_value(&mut cfg.style, choice, choice.title());
            }
        });
        ui.checkbox(&mut cfg.lyrics, "Lyrics over it");
        if full {
            ui.checkbox(&mut cfg.record, "Spinning record");
        } else {
            ui.checkbox(
                &mut cfg.fill_when_no_lyrics,
                "Fill the lyrics panel when a track has none",
            );
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
        let size = rect.width().min(rect.height());
        let (centre, radius) = (rect.center(), size * 0.17);
        let with_record = cfg.record && self.current().is_some();
        let hole = (cfg.style == VizStyle::Ring).then_some(if with_record {
            radius * 1.3
        } else {
            size * 0.2
        });
        self.viz
            .paint(&painter, rect, &cfg, self.accent, self.beat, 1.0, hole);
        if with_record && let Some(track) = self.current().cloned() {
            let art = track
                .artwork
                .as_deref()
                .and_then(|path| self.art.texture(path));
            let label = style::seed_color(&format!("{}{}", track.artist, track.album));
            let pulse = radius * (1.0 + 0.03 * self.beat);
            style::record(&painter, centre, pulse, art, label, self.spin);
        }
        if cfg.lyrics {
            self.viz_lyrics(&painter, rect);
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

    /// The line being sung, large near the top, with its neighbours dimmer.
    fn viz_lyrics(&mut self, painter: &Painter, rect: Rect) {
        let dir = self.data_dir.join("lyrics");
        let Some(track) = self.current().cloned() else {
            return;
        };
        self.lyrics
            .update(painter.ctx(), &track, &dir, self.state.lyrics_online);
        let Status::Found(Lyrics::Synced(lines)) = &self.lyrics.status else {
            return;
        };
        let now = self.reactive.now();
        let Some(current) = lines.partition_point(|(t, _)| *t <= now).checked_sub(1) else {
            return;
        };
        let size = rect.width().min(rect.height());
        let big = (size * 0.06).clamp(20.0, 44.0);
        let at = |k: isize| {
            lines
                .get(current.checked_add_signed(k)?)
                .filter(|l| !l.1.is_empty())
        };
        let rows = [(-1, 0.55, 0.62), (0, 1.0, 1.0), (1, 0.5, 0.62)];
        let y = rect.top() + rect.height() * 0.1;
        let mut cursor = y;
        for (offset, alpha, scale) in rows {
            let Some((_, text)) = at(offset) else {
                continue;
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
                rect.width() * 0.8,
            );
            let top = cursor;
            cursor += galley.size().y + 8.0;
            painter.add(egui::epaint::TextShape::new(
                pos2(rect.center().x - galley.size().x / 2.0, top),
                galley,
                colour,
            ));
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
