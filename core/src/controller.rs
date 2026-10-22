//! Ties the pieces together: hotkey -> selection -> wheel -> chosen slice -> job -> toast.
//!
//! Runs on its own thread and blocks on the hotkey channel, so it costs nothing while idle. The
//! cursor is only polled while the wheel is open.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::POINT;
use windows::Win32::Graphics::Gdi::{MonitorFromPoint, MONITOR_DEFAULTTONEAREST};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_ESCAPE};
use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;

use crate::config::Config;
use crate::hotkey::HotkeyEvent;
use crate::jobs::{self, JobError};
use crate::log::log;
use crate::overlay::{Overlay, Placement, WheelContent};
use crate::packs::Catalog;
use crate::selection::{foreground_window, selected_paths, Selection};
use crate::supervisor::{CancelToken, SupervisorError};
use crate::theme::{pick, Theme};
use crate::toast::{Toast, Toaster};
use crate::wheel::{hit_test, Slice};

/// How often the cursor is checked while the wheel is open (about 60 Hz).
const TRACK_INTERVAL: Duration = Duration::from_millis(16);

/// State the controller shares with the Settings window, which can change it at any time.
pub struct Shared {
    pub catalog: RwLock<Catalog>,
    pub config: RwLock<Config>,
    /// Installed wheel themes; always holds at least the built-in default.
    pub themes: Vec<Theme>,
}

pub struct Controller {
    overlay: Arc<dyn Overlay>,
    shared: Arc<Shared>,
    jobs: Sender<Job>,
}

struct Job {
    slice_id: String,
    paths: Vec<PathBuf>,
}

impl Controller {
    /// Starts the controller and job threads.
    pub fn start(
        overlay: Arc<dyn Overlay>,
        events: Receiver<HotkeyEvent>,
        shared: Arc<Shared>,
        toaster: Arc<dyn Toaster>,
    ) {
        let (jobs_tx, jobs_rx) = mpsc::channel();
        let job_shared = shared.clone();
        std::thread::Builder::new()
            .name("flick-jobs".into())
            .spawn(move || run_jobs(jobs_rx, job_shared, toaster))
            .expect("spawn job thread");

        let controller = Controller {
            overlay,
            shared,
            jobs: jobs_tx,
        };
        std::thread::Builder::new()
            .name("flick-controller".into())
            .spawn(move || {
                while let Ok(event) = events.recv() {
                    if event == HotkeyEvent::Pressed {
                        controller.session(&events);
                    }
                }
            })
            .expect("spawn controller thread");
    }

    /// One press-hold-release of the shortcut.
    fn session(&self, events: &Receiver<HotkeyEvent>) {
        let started = Instant::now();
        let window = foreground_window();
        let centre = cursor();

        let selection = selected_paths(window).unwrap_or_else(|e| {
            log(&format!("selection failed: {e}"));
            Selection::default()
        });
        let selection_ms = ms(started.elapsed());
        let (slices, theme) = {
            let catalog = self.shared.catalog.read().unwrap();
            let config = self.shared.config.read().unwrap();
            let slices: Vec<Slice> = catalog
                .slices_for(&selection.paths, &config.slices)
                .into_iter()
                .map(Slice::from)
                .collect();
            (
                slices,
                Arc::new(pick(&self.shared.themes, &config.theme).clone()),
            )
        };
        let content = WheelContent {
            caption: caption(&selection, slices.is_empty()),
            slices: slices.clone(),
            theme,
        };
        let placement = Placement {
            x: centre.x,
            y: centre.y,
            scale: scale_at(centre),
        };
        if let Err(e) = self.overlay.show(placement, &content) {
            log(&format!("wheel failed to open: {e}"));
            return;
        }
        log(&format!(
            "wheel open: {:.1} ms total, selection {selection_ms:.1} ms, {} path(s), {} slice(s)",
            ms(started.elapsed()),
            selection.paths.len(),
            slices.len()
        ));

        let mut highlighted = None;
        loop {
            match events.recv_timeout(TRACK_INTERVAL) {
                Ok(HotkeyEvent::Released { .. }) => break,
                Ok(HotkeyEvent::Pressed) => {}
                Err(RecvTimeoutError::Timeout) => {
                    if unsafe { GetAsyncKeyState(VK_ESCAPE.0 as i32) } < 0 {
                        highlighted = None;
                        // Wait for the release so it does not start a new session.
                        let _ = events.recv();
                        break;
                    }
                    let now = cursor();
                    let dx = (now.x - centre.x) as f64 / placement.scale;
                    let dy = (now.y - centre.y) as f64 / placement.scale;
                    let hit = hit_test(dx, dy, slices.len());
                    if hit != highlighted {
                        highlighted = hit;
                        self.overlay.highlight(hit);
                    }
                }
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        self.overlay.hide();

        if let Some(slice) = highlighted.map(|i| &slices[i]) {
            let _ = self.jobs.send(Job {
                slice_id: slice.id.clone(),
                paths: selection.paths,
            });
        }
    }
}

/// Runs jobs one at a time, so a weak PC never has two heavy conversions at once.
fn run_jobs(jobs: Receiver<Job>, shared: Arc<Shared>, toaster: Arc<dyn Toaster>) {
    while let Ok(job) = jobs.recv() {
        // Copy what the job needs, so Settings can change the catalog while it runs.
        let found = {
            let catalog = shared.catalog.read().unwrap();
            catalog
                .find(&job.slice_id)
                .map(|(pack, slice)| (pack.clone(), slice.clone()))
        };
        let Some((pack, slice)) = found else {
            log(&format!("slice {} is no longer installed", job.slice_id));
            continue;
        };

        let started = Instant::now();
        let cancel = CancelToken::default();
        toaster.show(Toast::Running {
            title: format!("{}\u{2026}", slice.label),
            detail: describe(&job.paths),
            cancel: cancel.clone(),
        });
        match jobs::run_slice(&pack, &slice, &job.paths, &cancel) {
            Ok(output) => {
                log(&format!(
                    "{} done in {:.0} ms: {}",
                    slice.id,
                    ms(started.elapsed()),
                    output.display()
                ));
                toaster.show(Toast::Done {
                    title: format!("{} done", slice.label),
                    output,
                });
            }
            Err(JobError::Supervisor(SupervisorError::Cancelled)) => {
                log(&format!("{} cancelled", slice.id));
                toaster.hide();
            }
            Err(e) => {
                log(&format!("{} failed: {e}", slice.id));
                toaster.show(Toast::Failed {
                    title: format!("{} failed", slice.label),
                    message: e.to_string(),
                });
            }
        }
    }
}

/// "3 files in Notes", for the running toast.
fn describe(paths: &[PathBuf]) -> String {
    let folder = paths
        .first()
        .and_then(|p| p.parent())
        .and_then(|p| p.file_name())
        .map(|n| format!(" in {}", n.to_string_lossy()))
        .unwrap_or_default();
    match paths {
        [one] => one
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        many => format!("{} files{folder}", many.len()),
    }
}

fn caption(selection: &Selection, no_slices: bool) -> String {
    let count = selection.paths.len();
    let pdfs = selection
        .paths
        .iter()
        .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("pdf")))
        .count();
    match count {
        0 => "Nothing selected".into(),
        _ if no_slices => "No actions for this selection".into(),
        1 => selection.paths[0]
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        n if n == pdfs => format!("{n} PDFs"),
        n => format!("{n} files"),
    }
}

fn cursor() -> POINT {
    let mut point = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut point);
    }
    point
}

/// Scale factor (DPI / 96) of the monitor under `point`.
pub fn scale_at(point: POINT) -> f64 {
    unsafe {
        let monitor = MonitorFromPoint(point, MONITOR_DEFAULTTONEAREST);
        let (mut x, mut y) = (96u32, 96u32);
        let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut x, &mut y);
        x as f64 / 96.0
    }
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}
