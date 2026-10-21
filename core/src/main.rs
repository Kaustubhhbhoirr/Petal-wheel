// Hide the console window in release builds; Flick lives in the tray.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use flick_core::controller::{scale_at, Controller, Notice};
use flick_core::hotkey::{HotkeyListener, Shortcut};
use flick_core::log::log;
use flick_core::overlay::native::NativeOverlay;
use flick_core::overlay::webview::{PaintSignal, WebviewOverlay};
use flick_core::overlay::{Overlay, Placement, WheelContent};
use flick_core::wheel::slices_for;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager, RunEvent};
use tauri_plugin_notification::NotificationExt;
use windows::Win32::Foundation::POINT;
use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Renderer {
    Native,
    Webview,
    WebviewWarm,
}

impl Renderer {
    fn parse(name: &str) -> Option<Self> {
        match name {
            "native" => Some(Self::Native),
            "webview" => Some(Self::Webview),
            "webview-warm" => Some(Self::WebviewWarm),
            _ => None,
        }
    }
}

/// Command line, mostly for the spike's measurements:
///
///   flick [--renderer native|webview|webview-warm] [--open-once]
///   flick --bench <renderer> [--runs N] [--out results.json]
struct Args {
    renderer: Renderer,
    bench: bool,
    runs: usize,
    out: Option<PathBuf>,
    open_once: bool,
}

impl Args {
    fn parse() -> Self {
        let mut args = Args {
            renderer: Renderer::Native,
            bench: false,
            runs: 20,
            out: None,
            open_once: false,
        };
        let mut it = std::env::args().skip(1);
        while let Some(arg) = it.next() {
            match arg.as_str() {
                "--renderer" | "--bench" => {
                    args.bench |= arg == "--bench";
                    let name = it.next().unwrap_or_default();
                    args.renderer = Renderer::parse(&name).unwrap_or_else(|| {
                        log(&format!("unknown renderer {name:?}, using native"));
                        Renderer::Native
                    });
                }
                "--runs" => args.runs = it.next().and_then(|n| n.parse().ok()).unwrap_or(20),
                "--out" => args.out = it.next().map(PathBuf::from),
                "--open-once" => args.open_once = true,
                other => log(&format!("ignoring unknown argument {other:?}")),
            }
        }
        args
    }
}

/// Keeps the hotkey registered for the life of the app.
struct HotkeyState(#[allow(dead_code)] Mutex<Option<HotkeyListener>>);

fn main() {
    let args = Args::parse();
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .manage(PaintSignal::default())
        .invoke_handler(tauri::generate_handler![
            flick_core::overlay::webview::wheel_painted
        ])
        .setup(move |app| {
            build_tray(app.handle())?;
            let overlay = make_overlay(app.handle(), args.renderer)?;
            if args.bench {
                let handle = app.handle().clone();
                let (runs, out, renderer) = (args.runs, args.out.clone(), args.renderer);
                std::thread::spawn(move || {
                    bench(&*overlay, renderer, runs, out);
                    handle.exit(0);
                });
                return Ok(());
            }
            if args.open_once {
                let overlay = overlay.clone();
                std::thread::spawn(move || open_once(&*overlay));
            }
            start_hotkey(app.handle(), overlay);
            Ok(())
        })
        .build(tauri::generate_context!())
        .unwrap_or_else(|e| {
            log(&format!("failed to start: {e}"));
            std::process::exit(1);
        });

    app.run(|_, event| {
        // The wheel window is destroyed after each use; that must not quit the tray app.
        if let RunEvent::ExitRequested {
            code: None, api, ..
        } = event
        {
            api.prevent_exit();
        }
    });
}

fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let quit = MenuItem::with_id(app, "quit", "Quit Flick", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&quit])?;
    TrayIconBuilder::with_id("main")
        .icon(app.default_window_icon().cloned().expect("bundled icon"))
        .tooltip("Flick")
        .menu(&menu)
        .on_menu_event(|app, event| {
            if event.id.as_ref() == "quit" {
                app.exit(0);
            }
        })
        .build(app)?;
    Ok(())
}

fn make_overlay(app: &AppHandle, renderer: Renderer) -> Result<Arc<dyn Overlay>, String> {
    let started = Instant::now();
    let overlay: Arc<dyn Overlay> = match renderer {
        Renderer::Native => Arc::new(NativeOverlay::start()?),
        Renderer::Webview | Renderer::WebviewWarm => {
            let overlay = WebviewOverlay::new(app.clone(), renderer == Renderer::WebviewWarm);
            overlay.prewarm()?;
            Arc::new(overlay)
        }
    };
    log(&format!(
        "renderer {renderer:?} ready in {:.1} ms",
        started.elapsed().as_secs_f64() * 1000.0
    ));
    Ok(overlay)
}

fn start_hotkey(app: &AppHandle, overlay: Arc<dyn Overlay>) {
    let shortcut = Shortcut::default();
    let (tx, rx) = mpsc::channel();
    let worker_dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(PathBuf::from))
        .unwrap_or_default();
    let notifier = app.clone();
    Controller::start(overlay, rx, worker_dir, move |notice| {
        toast(&notifier, notice)
    });

    match HotkeyListener::start(shortcut, move |event| {
        let _ = tx.send(event);
    }) {
        Ok(listener) => {
            log(&format!("listening for {shortcut}"));
            app.manage(HotkeyState(Mutex::new(Some(listener))));
        }
        Err(e) => {
            log(&e.to_string());
            toast(
                app,
                Notice {
                    title: "Flick shortcut unavailable".into(),
                    body: e.to_string(),
                },
            );
        }
    }
}

fn toast(app: &AppHandle, notice: Notice) {
    if let Err(e) = app
        .notification()
        .builder()
        .title(&notice.title)
        .body(&notice.body)
        .show()
    {
        log(&format!("toast failed: {e}"));
    }
}

fn screen_centre() -> Placement {
    let (w, h) = unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) };
    let (x, y) = (w / 2, h / 2);
    Placement {
        x,
        y,
        scale: scale_at(POINT { x, y }),
    }
}

fn sample_content() -> WheelContent {
    WheelContent {
        slices: slices_for(&["a.pdf", "b.pdf"]),
        caption: "2 PDFs".into(),
    }
}

/// Shows and hides the wheel once, so idle measurements reflect a Flick that has been used.
fn open_once(overlay: &dyn Overlay) {
    std::thread::sleep(Duration::from_secs(2));
    if overlay.show(screen_centre(), &sample_content()).is_ok() {
        std::thread::sleep(Duration::from_millis(300));
        overlay.hide();
        log("open-once done");
    }
}

/// Opens and closes the wheel `runs` times and records how long each open took.
fn bench(overlay: &dyn Overlay, renderer: Renderer, runs: usize, out: Option<PathBuf>) {
    let content = sample_content();
    let at = screen_centre();
    let mut times = Vec::with_capacity(runs);
    std::thread::sleep(Duration::from_millis(500));
    for _ in 0..runs {
        let started = Instant::now();
        if let Err(e) = overlay.show(at, &content) {
            log(&format!("bench: show failed: {e}"));
            return;
        }
        times.push(started.elapsed().as_secs_f64() * 1000.0);
        for i in [Some(0), Some(1), None] {
            std::thread::sleep(Duration::from_millis(60));
            overlay.highlight(i);
        }
        std::thread::sleep(Duration::from_millis(60));
        overlay.hide();
        std::thread::sleep(Duration::from_millis(300));
    }

    let first = times[0];
    let mut rest = times[1..].to_vec();
    rest.sort_by(f64::total_cmp);
    let pick = |q: f64| {
        rest.get(((rest.len() as f64 - 1.0) * q).round() as usize)
            .copied()
    };
    let result = serde_json::json!({
        "renderer": format!("{renderer:?}"),
        "runs": runs,
        "first_open_ms": first,
        "median_ms": pick(0.5),
        "p95_ms": pick(0.95),
        "max_ms": rest.last(),
        "all_ms": times,
    });
    log(&format!("bench: {result}"));
    if let Some(path) = out {
        let _ = std::fs::write(path, serde_json::to_string_pretty(&result).unwrap());
    }
}
