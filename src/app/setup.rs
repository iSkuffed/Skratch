//! The welcome / setup page: music folder, mpv and the optional download tools.
use super::Skratch;
use crate::style;
use eframe::egui::{self, Color32, CornerRadius, RichText};
use skratch::tools::{self, Msg, Status, Step};
use std::sync::mpsc;

const BAD: Color32 = Color32::from_rgb(230, 100, 90);

pub struct Setup {
    downloads: bool,
    job: Option<mpsc::Receiver<Msg>>,
    /// Close the page once the running job ends without errors.
    then_close: bool,
    log: String,
    errors: Vec<String>,
}

impl Setup {
    pub fn new(downloads: bool) -> Self {
        Self {
            downloads,
            job: None,
            then_close: false,
            log: String::new(),
            errors: Vec::new(),
        }
    }
}

fn step_name(step: Step) -> &'static str {
    match step {
        Step::Mpv => "mpv",
        Step::YtDlp | Step::UpdateYtDlp => "yt-dlp",
        Step::Ffmpeg => "ffmpeg",
    }
}

fn missing_downloads(tools: &Status) -> Vec<Step> {
    let mut steps = Vec::new();
    if tools.yt_dlp.is_none() {
        steps.push(Step::YtDlp);
    }
    if !tools.ffmpeg {
        steps.push(Step::Ffmpeg);
    }
    steps
}

fn ticked(ui: &mut egui::Ui, text: &str) {
    ui.add_enabled(false, egui::Checkbox::new(&mut true, text));
}

fn row_title(ui: &mut egui::Ui, text: &str) {
    ui.add_space(12.0);
    ui.separator();
    ui.label(RichText::new(text).size(16.0).strong().color(style::CREAM));
    ui.add_space(2.0);
}

impl Skratch {
    pub(super) fn open_setup(&mut self, downloads: bool) {
        self.refresh_tools();
        self.setup = Some(Setup::new(downloads));
    }

    pub(super) fn refresh_tools(&mut self) {
        self.tools = tools::detect(&self.data_dir);
        if let Some(program) = self.tools.yt_dlp.clone() {
            self.downloader
                .set_tools(program, self.tools.ffmpeg_dir.clone());
        }
    }

    fn close_setup(&mut self) {
        self.setup = None;
        self.state.setup_done = true;
        self.save();
    }

    fn start_job(&mut self, steps: Vec<Step>, then_close: bool) {
        let Some(setup) = self.setup.as_mut() else {
            return;
        };
        let (tx, rx) = mpsc::channel();
        setup.job = Some(rx);
        setup.then_close = then_close;
        setup.log = "Starting...".into();
        setup.errors.clear();
        tools::run(self.data_dir.clone(), steps, tx, self.wake.clone());
    }

    pub(super) fn poll_setup(&mut self) {
        let Some(setup) = self.setup.as_mut() else {
            return;
        };
        let Some(rx) = setup.job.as_ref() else {
            return;
        };
        let mut finished = false;
        loop {
            match rx.try_recv() {
                Ok(Msg::Log(line)) => setup.log = line,
                Ok(Msg::Done(_)) => {}
                Ok(Msg::Failed(step, error)) => {
                    setup.errors.push(format!("{}: {error}", step_name(step)))
                }
                Ok(Msg::Finished) | Err(mpsc::TryRecvError::Disconnected) => {
                    finished = true;
                    break;
                }
                Err(mpsc::TryRecvError::Empty) => break,
            }
        }
        if !finished {
            return;
        }
        setup.job = None;
        let close = setup.then_close && setup.errors.is_empty();
        self.refresh_tools();
        if close {
            self.close_setup();
        }
    }

    pub(super) fn setup_window(&mut self, ctx: &egui::Context) {
        if self.setup.is_none() {
            return;
        }
        ctx.layer_painter(egui::LayerId::new(
            egui::Order::Middle,
            egui::Id::new("setup-dim"),
        ))
        .rect_filled(
            ctx.content_rect(),
            CornerRadius::same(style::RADIUS),
            Color32::from_black_alpha(170),
        );
        egui::Window::new("setup")
            .order(egui::Order::Foreground)
            .frame(egui::Frame::window(&ctx.style()).inner_margin(egui::Margin::same(22)))
            .title_bar(false)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .fixed_size([540.0, 0.0])
            .show(ctx, |ui| self.setup_contents(ui));
    }

    fn setup_contents(&mut self, ui: &mut egui::Ui) {
        let busy = self.setup.as_ref().is_some_and(|s| s.job.is_some());
        ui.add_space(4.0);
        ui.label(
            RichText::new("Welcome to Skratch")
                .size(26.0)
                .strong()
                .color(style::CREAM),
        );
        ui.label(
            RichText::new(
                "Let's get everything ready. Anything already on your computer is ticked.",
            )
            .weak(),
        );

        row_title(ui, "Music folder");
        if let Some(first) = self.sources.first() {
            let name = first
                .file_name()
                .unwrap_or(first.as_os_str())
                .to_string_lossy();
            let more = self.sources.len() - 1;
            let more = if more > 0 {
                format!(" and {more} more")
            } else {
                String::new()
            };
            ticked(ui, &format!("Using {name}{more}"));
        } else {
            ui.label("Where your music lives. Skratch never moves or copies it.");
            if ui
                .add_enabled(
                    self.folder_rx.is_none(),
                    egui::Button::new("Choose folder..."),
                )
                .clicked()
            {
                self.pick_folder();
            }
        }

        row_title(ui, "mpv (plays your music)");
        if self.tools.mpv {
            ticked(ui, "Already installed");
        } else {
            ui.label("Skratch plays audio through mpv, and it isn't installed yet.");
            let line = tools::mpv_shell_line();
            ui.horizontal(|ui| {
                let can = line.is_some() && tools::can_ask_for_password() && !busy;
                if ui
                    .add_enabled(can, egui::Button::new("Install mpv"))
                    .on_hover_text("Your system asks for your password. Skratch never sees it.")
                    .clicked()
                {
                    self.start_job(vec![Step::Mpv], false);
                }
                if ui
                    .add_enabled(!busy, egui::Button::new("Check again"))
                    .clicked()
                {
                    self.refresh_tools();
                }
            });
            match line {
                Some(line) => {
                    ui.label(
                        RichText::new("Or run this in a terminal, then press Check again:")
                            .small()
                            .weak(),
                    );
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&line).monospace());
                        if ui.small_button("Copy").clicked() {
                            ui.ctx().copy_text(line.clone());
                        }
                    });
                }
                None => {
                    ui.label(
                        RichText::new("Install mpv with your distribution's package manager, then press Check again.")
                            .small()
                            .weak(),
                    );
                }
            }
        }

        row_title(ui, "Downloads (optional)");
        let steps = missing_downloads(&self.tools);
        if steps.is_empty() {
            ticked(ui, "Already installed");
            if self.tools.yt_dlp_managed
                && ui
                    .add_enabled(!busy, egui::Button::new("Update yt-dlp"))
                    .on_hover_text("Sites change often; an old yt-dlp stops working.")
                    .clicked()
            {
                self.start_job(vec![Step::UpdateYtDlp], false);
            }
        } else if let Some(setup) = self.setup.as_mut() {
            ui.add_enabled(
                !busy,
                egui::Checkbox::new(&mut setup.downloads, "Let me download music from links"),
            );
            ui.label(RichText::new("Fetches what is missing:").small().weak());
            if steps.contains(&Step::YtDlp) {
                ui.label(RichText::new("   yt-dlp, grabs the audio. About 40 MB.").small());
            }
            if steps.contains(&Step::Ffmpeg) {
                ui.label(
                    RichText::new("   ffmpeg, converts audio and adds tags and cover art. About 70 MB to download, 220 MB once unpacked.")
                        .small(),
                );
            }
            ui.label(
                RichText::new("From the official releases, checked against their checksums, and kept in Skratch's own folder. Nothing is installed system-wide. You can also do this later from the Downloads tab.")
                    .small()
                    .weak(),
            );
        }

        let Some(setup) = self.setup.as_ref() else {
            return;
        };
        let (log, errors) = (setup.log.clone(), setup.errors.clone());
        let install = setup.downloads && !steps.is_empty();
        ui.add_space(10.0);
        if busy {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(log);
            });
        }
        for error in &errors {
            ui.colored_label(BAD, error);
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let label = if install {
                "Install and get started"
            } else {
                "Get started"
            };
            if ui
                .add_enabled(!busy, egui::Button::new(RichText::new(label).strong()))
                .clicked()
            {
                if install {
                    self.start_job(steps, true);
                } else {
                    self.close_setup();
                }
            }
            if ui
                .add_enabled(!busy, egui::Button::new("Skip for now"))
                .clicked()
            {
                self.close_setup();
            }
        });
    }
}
