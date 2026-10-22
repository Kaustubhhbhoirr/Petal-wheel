//! Ties the pieces together: hotkey -> selection -> wheel -> chosen slice -> job -> toast.
//!
//! Runs on its own thread and blocks on the hotkey channel, so it costs nothing while idle. The
//! cursor is only polled while the wheel is open.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::POINT;
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_ESCAPE};
use windows::Win32::UI::WindowsAndMessaging::{GetCursorPos, SetCursorPos};

use crate::config::Config;
use crate::hotkey::HotkeyEvent;
use crate::jobs::{self, JobError, Summary};
use crate::log::log;
use crate::mouse::RightClicks;
use crate::overlay::{Overlay, Placement, WheelContent};
use crate::packs::Catalog;
use crate::selection::{foreground_window, selected_paths, Selection};
use crate::supervisor::CancelToken;
use crate::theme::{pick, Theme};
use crate::toast::{Toast, Toaster};
use crate::wheel::{build_menu, reach, Kind, Navigator, Slice, WheelState};

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
        let pointer = cursor();

        let selection = selected_paths(window).unwrap_or_else(|e| {
            log(&format!("selection failed: {e}"));
            Selection::default()
        });
        let selection_ms = ms(started.elapsed());
        let (slices, theme, delay) = {
            let catalog = self.shared.catalog.read().unwrap();
            let config = self.shared.config.read().unwrap();
            let slices: Vec<Slice> =
                build_menu(&catalog.tree_for(&selection.paths, &config.slices));
            (
                slices,
                Arc::new(pick(&self.shared.themes, &config.theme).clone()),
                Duration::from_millis(config.sub_ring_delay_ms()),
            )
        };
        let content = WheelContent {
            caption: caption(&selection, slices.is_empty()),
            slices: slices.clone(),
            theme,
        };
        let style = content.theme.style;
        let scale = scale_at(pointer);

        // Near a screen edge, move the wheel (and the pointer with it) inward so the main ring
        // and any sub-ring stay fully on screen.
        let centre = keep_on_screen(pointer, reach(style) * scale);
        if centre != pointer {
            unsafe {
                let _ = SetCursorPos(centre.x, centre.y);
            }
        }
        let placement = Placement {
            x: centre.x,
            y: centre.y,
            scale,
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

        // Right-clicks close a sub-ring while the wheel is open (and nothing else gets them).
        let (clicks_tx, clicks) = mpsc::channel();
        let right_clicks = RightClicks::start(clicks_tx);
        let mut nav = Navigator::new(style, delay);
        let mut shown = WheelState::default();
        let mut escape_was_down = false;
        let cancelled = 'track: loop {
            match events.recv_timeout(TRACK_INTERVAL) {
                Ok(HotkeyEvent::Released { .. }) => break false,
                Ok(HotkeyEvent::Pressed) => {}
                Err(RecvTimeoutError::Timeout) => {
                    let escape = unsafe { GetAsyncKeyState(VK_ESCAPE.0 as i32) } < 0;
                    let backs = clicks.try_iter().count() + usize::from(escape && !escape_was_down);
                    escape_was_down = escape;
                    for _ in 0..backs {
                        // Each Esc or right-click closes one level; with none open, it cancels.
                        if !nav.back() {
                            // Wait for the release so it does not start a new session.
                            let _ = events.recv();
                            break 'track true;
                        }
                    }
                    let now = cursor();
                    let dx = (now.x - centre.x) as f64 / scale;
                    let dy = (now.y - centre.y) as f64 / scale;
                    let state = nav.track(&slices, dx, dy, Instant::now());
                    if state != shown {
                        shown = state;
                        self.overlay.update(state);
                    }
                }
                Err(RecvTimeoutError::Disconnected) => break true,
            }
        };
        drop(right_clicks);
        self.overlay.hide();
        if cancelled {
            return;
        }

        // A hovered child runs; a hovered parent runs its default child, if any; the hub cancels.
        match nav.picked(&slices) {
            Some(slice) if slice.kind == Kind::App => {
                // App slices need input from the Flick app window before anything runs.
                log(&format!("{} needs the app window", slice.id));
            }
            Some(slice) if slice.kind == Kind::Run => {
                let _ = self.jobs.send(Job {
                    slice_id: slice.id.clone(),
                    paths: selection.paths,
                });
            }
            _ => {}
        }
    }
}

/// The wheel centre for a pointer at `point`: unchanged unless that would put part of a wheel
/// reaching `reach` physical pixels outside the monitor's work area.
fn keep_on_screen(point: POINT, reach: f64) -> POINT {
    let reach = reach.ceil() as i32;
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    let work = unsafe {
        let monitor = MonitorFromPoint(point, MONITOR_DEFAULTTONEAREST);
        if !GetMonitorInfoW(monitor, &mut info).as_bool() {
            return point;
        }
        info.rcWork
    };
    let clamp = |v: i32, lo: i32, hi: i32| {
        if hi - lo < 2 * reach {
            (lo + hi) / 2
        } else {
            v.clamp(lo + reach, hi - reach)
        }
    };
    POINT {
        x: clamp(point.x, work.left, work.right),
        y: clamp(point.y, work.top, work.bottom),
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
            Ok(outcome) => {
                log(&format!(
                    "{} done in {:.0} ms: {} file(s), first {}",
                    slice.id,
                    ms(started.elapsed()),
                    outcome.outputs.len(),
                    outcome.outputs[0].display()
                ));
                for (input, error) in &outcome.failed {
                    log(&format!(
                        "{} failed on {}: {error}",
                        slice.id,
                        input.display()
                    ));
                }
                toaster.show(match jobs::summarize(&slice.label, &outcome) {
                    Summary::Done { title, output } => Toast::Done { title, output },
                    Summary::Partial { title, message } => Toast::Failed { title, message },
                });
            }
            Err(JobError::Cancelled) => {
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
