//! The window's app: the interface behind a lock, with a thread that keeps playback going while
//! no frames are drawn.
//!
//! A minimised window gets no frame callbacks on Wayland, so the drawing loop stalls (egui issue
//! #5136), and with it everything that ran once per frame: a track that ends would not start the
//! next one, and the media keys and the desktop's now-playing widget would not answer until the
//! window came back. The watcher notices that frames have stopped and does that work itself.
use super::Skratch;
use eframe::egui;
use std::{
    path::PathBuf,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
    time::{Duration, Instant},
};

/// How often the watcher looks, and how long without a frame counts as frames having stopped.
/// A window with nothing moving still draws once a second, so it is also the most the watcher
/// ever does for one that is showing; it only handles what is already waiting.
const WATCH_EVERY: Duration = Duration::from_millis(200);
const STALLED_AFTER: Duration = Duration::from_millis(600);

/// What the watcher needs from the app it keeps going.
trait Watched {
    /// When the last frame began.
    fn last_frame(&self) -> Instant;
    /// Does the playback work a frame would have done. True when the window should redraw.
    fn step(&mut self, ctx: &egui::Context) -> bool;
}

impl Watched for Skratch {
    fn last_frame(&self) -> Instant {
        self.frame_at
    }

    fn step(&mut self, ctx: &egui::Context) -> bool {
        self.keep_playing(ctx)
    }
}

/// Starts the watcher thread: every `every`, if no frame has begun for `stalled`, it does the
/// playback work itself. It ends once the app has been dropped.
fn watch<T: Watched + Send + 'static>(
    app: &Arc<Mutex<T>>,
    ctx: egui::Context,
    every: Duration,
    stalled: Duration,
) {
    let app = Arc::downgrade(app);
    // without a watcher the app still works, it just stalls when minimised as it always did
    let _ = std::thread::Builder::new()
        .name("playback-watch".into())
        .spawn(move || {
            loop {
                std::thread::sleep(every);
                let Some(app) = app.upgrade() else { return };
                let Ok(mut app) = app.lock() else { return };
                if app.last_frame().elapsed() >= stalled && app.step(&ctx) {
                    ctx.request_repaint();
                }
            }
        });
}

/// What `eframe` runs: the interface, shared with the watcher.
pub struct Shell(Arc<Mutex<Skratch>>);

impl Shell {
    pub fn new(cc: &eframe::CreationContext<'_>, data_dir: PathBuf) -> Self {
        let app = Arc::new(Mutex::new(Skratch::new(cc, data_dir)));
        watch(&app, cc.egui_ctx.clone(), WATCH_EVERY, STALLED_AFTER);
        Self(app)
    }

    fn app(&self) -> MutexGuard<'_, Skratch> {
        // a panic on the watcher must not take the window down with it
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl eframe::App for Shell {
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        eframe::App::raw_input_hook(&mut *self.app(), ctx, raw_input);
    }

    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        eframe::App::update(&mut *self.app(), ctx, frame);
    }

    fn clear_color(&self, visuals: &egui::Visuals) -> [f32; 4] {
        eframe::App::clear_color(&*self.app(), visuals)
    }

    fn on_exit(&mut self, gl: Option<&eframe::glow::Context>) {
        eframe::App::on_exit(&mut *self.app(), gl);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Fake {
        frame: Instant,
        steps: Arc<AtomicUsize>,
    }

    impl Watched for Fake {
        fn last_frame(&self) -> Instant {
            self.frame
        }

        fn step(&mut self, _: &egui::Context) -> bool {
            self.steps.fetch_add(1, Ordering::SeqCst);
            true
        }
    }

    #[test]
    fn the_watcher_steps_in_only_once_frames_have_stopped() {
        let steps = Arc::new(AtomicUsize::new(0));
        let app = Arc::new(Mutex::new(Fake {
            frame: Instant::now(),
            steps: steps.clone(),
        }));
        watch(
            &app,
            egui::Context::default(),
            Duration::from_millis(5),
            Duration::from_millis(300),
        );
        // frames keep coming, so the watcher leaves everything to them
        for _ in 0..20 {
            std::thread::sleep(Duration::from_millis(5));
            app.lock().unwrap().frame = Instant::now();
        }
        assert_eq!(steps.load(Ordering::SeqCst), 0);
        // the window is minimised: frames stop and the watcher takes over
        std::thread::sleep(Duration::from_millis(700));
        assert!(steps.load(Ordering::SeqCst) > 0);
    }
}
