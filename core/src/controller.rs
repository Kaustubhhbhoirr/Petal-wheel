//! Ties the pieces together: hotkey -> selection -> wheel -> chosen slice -> job -> toast.
//!
//! Runs on its own thread and blocks on the hotkey channel, so it costs nothing while idle. The
//! cursor is only polled while the wheel is open.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::POINT;
use windows::Win32::Graphics::Gdi::{MonitorFromPoint, MONITOR_DEFAULTTONEAREST};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_ESCAPE};
use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;

use crate::hotkey::HotkeyEvent;
use crate::jobs;
use crate::log::log;
use crate::overlay::{Overlay, Placement, WheelContent};
use crate::selection::{foreground_window, selected_paths, Selection};
use crate::supervisor::{CancelToken, Limits};
use crate::wheel::{hit_test, slices_for, Slice};

/// How often the cursor is checked while the wheel is open (about 60 Hz).
const TRACK_INTERVAL: Duration = Duration::from_millis(16);

/// Something to tell the user, shown as a toast.
#[derive(Debug, Clone)]
pub struct Notice {
    pub title: String,
    pub body: String,
}

pub struct Controller {
    overlay: Arc<dyn Overlay>,
    jobs: Sender<Job>,
}

struct Job {
    slice: Slice,
    paths: Vec<PathBuf>,
}

impl Controller {
    /// Starts the controller and job threads. `notify` is called from the job thread.
    pub fn start(
        overlay: Arc<dyn Overlay>,
        events: Receiver<HotkeyEvent>,
        worker_dir: PathBuf,
        notify: impl Fn(Notice) + Send + 'static,
    ) {
        let (jobs_tx, jobs_rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("flick-jobs".into())
            .spawn(move || run_jobs(jobs_rx, worker_dir, notify))
            .expect("spawn job thread");

        let controller = Controller {
            overlay,
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
        let slices = slices_for(&selection.paths);
        let content = WheelContent {
            caption: caption(&selection),
            slices: slices.clone(),
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

        if let Some(slice) = highlighted.map(|i| slices[i].clone()) {
            if slice.enabled {
                let _ = self.jobs.send(Job {
                    slice,
                    paths: selection.paths,
                });
            }
        }
    }
}

/// Runs jobs one at a time, so a weak PC never has two heavy conversions at once.
fn run_jobs(jobs: Receiver<Job>, worker_dir: PathBuf, notify: impl Fn(Notice)) {
    let pdf_light = worker_dir.join("pdf-light.exe");
    while let Ok(job) = jobs.recv() {
        let started = Instant::now();
        let result = match job.slice.id {
            "pdf.merge" => jobs::merge_pdfs(
                &pdf_light,
                &job.paths,
                &Limits::default(),
                &CancelToken::default(),
            ),
            other => {
                log(&format!("no handler for slice {other}"));
                continue;
            }
        };
        let notice = match result {
            Ok(output) => {
                log(&format!(
                    "{} done in {:.0} ms: {}",
                    job.slice.id,
                    ms(started.elapsed()),
                    output.display()
                ));
                Notice {
                    title: format!("{} done", job.slice.label),
                    body: format!(
                        "Saved {}",
                        output.file_name().unwrap_or_default().to_string_lossy()
                    ),
                }
            }
            Err(e) => {
                log(&format!("{} failed: {e}", job.slice.id));
                Notice {
                    title: format!("{} failed", job.slice.label),
                    body: e.to_string(),
                }
            }
        };
        notify(notice);
    }
}

fn caption(selection: &Selection) -> String {
    let count = selection.paths.len();
    let pdfs = selection
        .paths
        .iter()
        .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("pdf")))
        .count();
    match count {
        0 => "Nothing selected".into(),
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
