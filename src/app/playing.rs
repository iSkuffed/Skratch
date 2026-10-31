//! The "Now playing" page: a big spinning record next to a panel the user can switch
//! between the track details, the queue and the whole library.
use super::{Action, Skratch, display, lyricview};
use crate::{
    state::{NowPanel, RhymeShow, Source},
    style,
};
use eframe::egui::text::{CCursor, CCursorRange};
use eframe::egui::{self, Align, RichText, Sense, pos2, vec2};
use skratch::{
    lyricfx,
    lyrics::{self, Lyrics, OtherState, Status},
    model::Track,
};

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
                self.panel_list(ui, NowPanel::Library);
                return;
            };
            let art = track
                .artwork
                .as_deref()
                .and_then(|path| self.art.texture(path));
            let label = style::seed_color(&format!("{}{}", track.artist, track.album));
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
                NowPanel::Lyrics => "LYRICS",
                NowPanel::UpNext => "UP NEXT",
                NowPanel::Library => "LIBRARY",
                NowPanel::Visualizer => "VISUALIZER",
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
                if panel == NowPanel::Visualizer {
                    let config = egui::containers::menu::MenuConfig::new()
                        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside);
                    egui::containers::menu::MenuButton::new("...")
                        .config(config)
                        .ui(ui, |ui| self.viz_menu(ui, false));
                    self.viz_full_button(ui);
                }
                if panel == NowPanel::Lyrics {
                    // stays open while display options are ticked; the actions close it themselves
                    let config = egui::containers::menu::MenuConfig::new()
                        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside);
                    egui::containers::menu::MenuButton::new("...")
                        .config(config)
                        .ui(ui, |ui| self.lyrics_menu(ui, track));
                }
            });
        });
        ui.add_space(4.0);
        let area = ui.available_rect_before_wrap();
        match panel {
            NowPanel::Details => self.details(ui, track),
            NowPanel::Lyrics => {
                // a track without lyrics gets the visualizer behind the "no lyrics" message
                let dir = self.data_dir.join("lyrics");
                self.lyrics
                    .update(ui.ctx(), track, &dir, self.state.lyric_sources());
                let none = matches!(self.lyrics.status, Status::Missing);
                if none
                    && self.lyrics.editing.is_none()
                    && self.state.visualizer.fill_when_no_lyrics
                {
                    self.viz_panel(ui, area, 0.28);
                }
                self.lyrics(ui, track);
            }
            NowPanel::Visualizer => {
                let dir = self.data_dir.join("lyrics");
                self.lyrics
                    .update(ui.ctx(), track, &dir, self.state.lyric_sources());
                let over = self.state.visualizer.lyrics
                    && matches!(self.lyrics.status, Status::Found(_))
                    && self.lyrics.editing.is_none();
                self.viz_panel(ui, area, if over { 0.3 } else { 1.0 });
                if over {
                    self.lyrics(ui, track);
                }
            }
            _ => self.panel_list(ui, panel),
        }
    }

    fn details(&mut self, ui: &mut egui::Ui, track: &skratch::model::Track) {
        ui.add_space(((ui.available_height() - 150.0) / 2.0).max(0.0));
        ui.label(
            RichText::new(&track.title)
                .font(style::display(36.0))
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

    /// Fix wrong lyrics: type them, bring them in from a file, drop them, or go back to the
    /// automatic ones. Kept in the app's data, so the music files stay as they are.
    fn lyrics_menu(&mut self, ui: &mut egui::Ui, track: &Track) {
        let dir = self.data_dir.join("lyrics");
        if ui.button("Edit lyrics").clicked() {
            self.lyrics.editing = Some(match &self.lyrics.status {
                Status::Found(found) => found.to_text(),
                _ => String::new(),
            });
            ui.close();
        }
        if ui.button("Import from file...").clicked() {
            self.import_lyrics_file(ui.ctx());
            ui.close();
        }
        if ui
            .button("Clear lyrics")
            .on_hover_text("Wrong lyrics: forget them and stop looking for this track")
            .clicked()
        {
            self.save_lyrics(&dir, track, "");
            ui.close();
        }
        let online = self.state.lyric_sources();
        let button = ui
            .add_enabled(
                online.allowed
                    && online.fallback
                    && self.lyrics.other_state != OtherState::Searching,
                egui::Button::new("Try another source"),
            )
            .on_hover_text(
                "Ask the other lyric services and switch to different lyrics if any turn up; \
                 otherwise these stay as they are. Needs online lookup and the NetEase fallback.",
            );
        if button.clicked() {
            self.lyrics.try_another(ui.ctx(), track, &dir);
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
        ui.separator();
        self.lyric_style_menu(ui);
        ui.separator();
        if lyrics::has_user_version(&dir, track)
            && ui
                .button("Back to automatic lyrics")
                .on_hover_text("Drop what was typed, imported or cleared and look again")
                .clicked()
        {
            lyrics::restore(&dir, track);
            self.lyrics.retry();
            ui.close();
        }
    }

    /// Every effect on the lyrics panel can be switched off here.
    fn lyric_style_menu(&mut self, ui: &mut egui::Ui) {
        let style = &mut self.state.lyric_style;
        ui.label(RichText::new("Display").small().color(style::CREAM_DIM));
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
        ui.horizontal(|ui| {
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

    fn save_lyrics(&mut self, dir: &std::path::Path, track: &Track, text: &str) {
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
        egui::ScrollArea::vertical()
            .max_height(room)
            .show(ui, |ui| {
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
                egui::ScrollArea::vertical()
                    .id_salt("lyrics")
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
                            lyricview::draw_line(ui, line, found, &spec, &look);
                        }
                    });
            }
            Status::Found(Lyrics::Synced(lines)) => {
                let current = lines.partition_point(|(t, _)| *t <= now).checked_sub(1);
                let marks = self.lyrics.marks.as_deref().unwrap_or(&[]);
                egui::ScrollArea::vertical()
                    .id_salt("lyrics")
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
                            let line = lyricview::draw_line(ui, text, found, &spec, &look)
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
                if self.state.lyrics_online {
                    let dir = self.data_dir.join("lyrics");
                    ui.label(dim("No lyrics found for this track.".into()));
                    if !self.lyrics.notes.is_empty() {
                        egui::CollapsingHeader::new("Show details")
                            .id_salt("lyric-notes")
                            .show(ui, |ui| {
                                for (source, said) in &self.lyrics.notes {
                                    ui.label(
                                        RichText::new(format!("{source}: {said}"))
                                            .color(style::CREAM_DIM),
                                    );
                                }
                            });
                    }
                    if !lyrics::has_user_version(&dir, track) && ui.button("Try again").clicked() {
                        self.lyrics.ask_again(&dir, track);
                    }
                } else {
                    ui.label(dim(
                        "No lyrics in the file or beside it (a .lrc file).".into()
                    ));
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new(
                            "Skratch can look them up on lrclib.net, a free community lyrics \
                             database. That sends the artist, title, album and length.",
                        )
                        .color(style::CREAM_DIM),
                    );
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Type lyrics").clicked() {
                        self.lyrics.editing = Some(String::new());
                    }
                    if ui.button("Import from file...").clicked() {
                        self.import_lyrics_file(ui.ctx());
                    }
                });
                ui.add_space(8.0);
                ui.checkbox(&mut self.state.lyrics_online, "Look up lyrics online");
            }
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
                    .id(super::SEARCH_ID.into())
                    .hint_text("Search library  (Ctrl+F)")
                    .desired_width(f32::INFINITY),
            );
            if search.changed() {
                self.rebuild();
            }
            // Search within what is playing: an album or playlist stays an album or playlist.
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
                    let rows = ids
                        .iter()
                        .filter_map(|id| self.by_id.get(id))
                        .filter(|i| matching.contains(i))
                        .enumerate()
                        .map(|(r, i)| (*i, None, r))
                        .collect();
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new("Searching what is playing")
                                .small()
                                .color(style::CREAM_DIM),
                        );
                        if ui.small_button("All tracks").clicked() {
                            self.state.source = Source::Library;
                        }
                    });
                    rows
                }
            }
        };
        let current = self.state.queue.current_id();
        let now = ui.input(|i| i.time);
        let bounds = ui.available_rect_before_wrap();
        let browsing = ui.rect_contains_pointer(bounds);
        let mut area = egui::ScrollArea::vertical()
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
                self.track_row(ui, index, queue, context);
            }
        });
        if browsing && ui.input(|i| i.pointer.any_click()) {
            self.list_hold = now + 1.0;
        }
    }
}
