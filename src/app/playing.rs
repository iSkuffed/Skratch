//! The "Now playing" page: a big spinning record next to a panel the user can switch
//! between the track details, the queue and the whole library.
use super::{Action, Skratch, display};
use crate::{state::NowPanel, style};
use eframe::egui::{self, RichText, Sense, pos2, vec2};

impl Skratch {
    pub(super) fn playing_ui(&mut self, ui: &mut egui::Ui) {
        egui::CentralPanel::default().show_inside(ui, |ui| {
            let Some(track) = self.current().cloned() else {
                ui.add_space(24.0);
                ui.heading("Nothing playing");
                ui.label("Pick a track below.");
                self.panel_list(ui, NowPanel::Library);
                return;
            };
            let art = track
                .artwork
                .as_deref()
                .and_then(|path| self.art.texture(path));
            let label = style::seed_color(&format!("{}{}", track.artist, track.album));
            let area = ui.available_rect_before_wrap();
            let r = (area.height() * 0.38)
                .min(area.width() * 0.19)
                .clamp(80.0, 200.0);
            let scale = r / 52.0;
            let centre = pos2(area.left() + r * 1.2 + 30.0, area.center().y);

            // the cover as a poster on the right, fading out towards the record
            if let Some(art) = art {
                let side = area.height() * 1.1;
                let poster = egui::Rect::from_min_size(
                    pos2(area.right() - side * 0.9, area.top() - side * 0.05),
                    vec2(side, side),
                );
                let mut mesh = egui::Mesh::with_texture(art);
                // three columns: transparent on the left, full tint from 60% across
                for (u, alpha) in [(0.0, 0), (0.6, 32), (1.0, 32)] {
                    let x = egui::lerp(poster.x_range(), u);
                    for v in [0.0, 1.0] {
                        mesh.vertices.push(egui::epaint::Vertex {
                            pos: pos2(x, egui::lerp(poster.y_range(), v)),
                            uv: pos2(u, v),
                            color: egui::Color32::from_white_alpha(alpha),
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

            // a faint halo in the track's colour that swells with the bass
            let beat =
                self.reactive
                    .level(ui.ctx(), &track.path, self.state.position, self.playing);
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
            style::record(ui.painter(), centre, r, art, label, self.spin);
            style::tonearm(
                ui.painter(),
                centre + vec2(92.0, -54.0) * scale,
                self.arm,
                self.accent,
                scale,
            );

            let left = centre.x + r * 1.9 + 30.0;
            let panel = egui::Rect::from_min_max(
                pos2(left, area.top() + 14.0),
                pos2(area.right() - 24.0, area.bottom() - 14.0),
            );
            ui.scope_builder(egui::UiBuilder::new().max_rect(panel), |ui| {
                ui.set_width(panel.width());
                self.panel_ui(ui, &track);
            });
        });
    }

    /// The switchable right-hand panel: a heading and drop-down, then the chosen content.
    fn panel_ui(&mut self, ui: &mut egui::Ui, track: &skratch::model::Track) {
        let panel = self.state.now_panel;
        ui.horizontal(|ui| {
            let heading = match panel {
                NowPanel::Details => "NOW PLAYING",
                NowPanel::UpNext => "UP NEXT",
                NowPanel::Library => "LIBRARY",
            };
            ui.label(
                RichText::new(heading)
                    .size(11.0)
                    .strong()
                    .color(style::CREAM_DIM),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                egui::ComboBox::from_id_salt("now-panel")
                    .selected_text(panel.title())
                    .width(110.0)
                    .show_ui(ui, |ui| {
                        for choice in NowPanel::ALL {
                            if ui
                                .selectable_value(&mut self.state.now_panel, choice, choice.title())
                                .clicked()
                            {
                                self.listed = None;
                            }
                        }
                    });
            });
        });
        ui.add_space(4.0);
        if panel == NowPanel::Details {
            self.details(ui, track);
        } else {
            self.panel_list(ui, panel);
        }
    }

    fn details(&mut self, ui: &mut egui::Ui, track: &skratch::model::Track) {
        ui.add_space(((ui.available_height() - 150.0) / 2.0).max(0.0));
        ui.label(
            RichText::new(&track.title)
                .size(36.0)
                .strong()
                .color(style::CREAM),
        );
        ui.label(
            RichText::new(display(&track.artist, "Unknown artist"))
                .size(22.0)
                .color(self.accent),
        );
        let album = [Some(track.album.clone()), track.year.map(|y| y.to_string())]
            .into_iter()
            .flatten()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join("  ·  ");
        if !album.is_empty() {
            ui.add_space(6.0);
            ui.label(RichText::new(album).size(16.0).color(style::CREAM_DIM));
        }
    }

    /// The queue or the whole library as clickable rows, scrolled to the playing track.
    fn panel_list(&mut self, ui: &mut egui::Ui, panel: NowPanel) {
        // (track index, position in the queue, position among the visible tracks)
        let rows: Vec<(usize, Option<usize>, usize)> = if panel == NowPanel::UpNext {
            self.state
                .queue
                .ids
                .iter()
                .enumerate()
                .filter_map(|(q, id)| self.by_id.get(id).map(|i| (*i, Some(q), 0)))
                .collect()
        } else {
            let search = ui.add(
                egui::TextEdit::singleline(&mut self.search)
                    .hint_text("Search library")
                    .desired_width(f32::INFINITY),
            );
            if search.changed() {
                self.rebuild();
            }
            self.filtered
                .iter()
                .enumerate()
                .map(|(r, i)| (*i, None, r))
                .collect()
        };
        let current = self.state.queue.current_id();
        let mut area = egui::ScrollArea::vertical()
            .id_salt(("now-list", panel as u8))
            .auto_shrink(false);
        if self.listed != current {
            self.listed = current;
            let at = rows
                .iter()
                .position(|(i, ..)| Some(self.tracks[*i].id) == current);
            if let Some(at) = at {
                area = area.vertical_scroll_offset((at as f32 * 40.0 - 80.0).max(0.0));
            }
        }
        area.show_rows(ui, 40.0, rows.len(), |ui, range| {
            for r in range {
                let (index, queue, context) = rows[r];
                self.track_row(ui, index, queue, context);
            }
        });
    }
}
