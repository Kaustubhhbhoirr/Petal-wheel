// Hide the console window in release builds; Flick lives in the tray.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, RwLock};
use std::time::{Duration, Instant};

use flick_core::config;
use flick_core::controller::{scale_at, Controller, Shared};
use flick_core::hotkey::{HotkeyError, HotkeyManager, Shortcut};
use flick_core::log::log;
use flick_core::overlay::native::NativeOverlay;
use flick_core::overlay::webview::{PaintSignal, WebviewOverlay};
use flick_core::overlay::{Overlay, Placement, WheelContent};
use flick_core::packs::{self, Catalog, PackError};
use flick_core::settings::{self, open_settings, AppState};
use flick_core::toast::{Toast, ToastWindow, Toaster};
use flick_core::wheel::Slice;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager, RunEvent};
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

/// Command line, mostly for testing and measurements:
///
///   flick [--renderer native|webview|webview-warm] [--shortcut "Ctrl+Shift+Q"] [--open-once]
///   flick --bench <renderer> [--runs N] [--out results.json]
///
/// `--shortcut` applies to this run only; Settings changes the saved one.
struct Args {
    renderer: Renderer,
    shortcut: Option<Shortcut>,
    bench: bool,
    runs: usize,
    out: Option<PathBuf>,
    open_once: bool,
}

impl Args {
    fn parse() -> Self {
        let mut args = Args {
            renderer: Renderer::Native,
            shortcut: None,
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
                "--shortcut" => {
                    let text = it.next().unwrap_or_default();
                    match text.parse() {
                        Ok(shortcut) => args.shortcut = Some(shortcut),
                        Err(e) => log(&format!("ignoring shortcut {text:?}: {e}")),
                    }
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

fn main() {
    let args = Args::parse();
    let app = tauri::Builder::default()
        .manage(PaintSignal::default())
        .invoke_handler(tauri::generate_handler![
            flick_core::overlay::webview::wheel_painted,
            settings::get_settings,
            settings::check_shortcut,
            settings::save_settings,
            settings::pause_hotkey,
            settings::close_settings,
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
            start(app.handle(), overlay, args.shortcut)?;
            Ok(())
        })
        .build(tauri::generate_context!())
        .unwrap_or_else(|e| {
            log(&format!("failed to start: {e}"));
            std::process::exit(1);
        });

    app.run(|_, event| {
        // Flick's windows come and go (wheel, Settings); none of them closing quits the tray app.
        if let RunEvent::ExitRequested {
            code: None, api, ..
        } = event
        {
            api.prevent_exit();
        }
    });
}

/// Loads config and packs, then starts the toast, the controller and the shortcut.
fn start(
    app: &AppHandle,
    overlay: Arc<dyn Overlay>,
    shortcut_override: Option<Shortcut>,
) -> Result<(), String> {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(PathBuf::from))
        .unwrap_or_default();

    let config_path = config::config_path();
    let config = match &config_path {
        Some(path) => {
            let (config, warning) = config::load(path);
            if let Some(warning) = warning {
                log(&warning);
            }
            config
        }
        None => config::Config::default(),
    };

    let packs_dir = packs::default_dir(&exe_dir);
    let catalog = load_packs(packs_dir.as_deref(), &exe_dir);

    let toaster: Arc<dyn Toaster> = Arc::new(ToastWindow::start()?);
    let shared = Arc::new(Shared {
        catalog: RwLock::new(catalog),
        config: RwLock::new(config.clone()),
    });

    let (tx, rx) = mpsc::channel();
    Controller::start(overlay, rx, shared.clone(), toaster.clone());

    let hotkeys = Arc::new(HotkeyManager::new(tx));
    let shortcut = shortcut_override.unwrap_or_else(|| config.shortcut());
    match hotkeys.set(shortcut) {
        Ok(()) => log(&format!("listening for {shortcut}")),
        Err(e) => {
            log(&e.to_string());
            hotkeys.remember(shortcut);
            let settings_app = app.clone();
            toaster.show(Toast::Notice {
                title: "Shortcut unavailable".into(),
                message: match e {
                    HotkeyError::Conflict(s) => format!("{s} is used by another app."),
                    other => other.to_string(),
                },
                button: "Change shortcut".into(),
                on_click: Arc::new(move || open_settings(&settings_app)),
            });
        }
    }

    {
        let catalog = shared.catalog.read().unwrap();
        if catalog.packs.is_empty() {
            let settings_app = app.clone();
            toaster.show(Toast::Notice {
                title: "No packs loaded".into(),
                message: "The wheel has no actions until a pack is installed.".into(),
                button: "Open Settings".into(),
                on_click: Arc::new(move || open_settings(&settings_app)),
            });
        }
    }

    app.manage(AppState {
        shared,
        hotkeys,
        config_path,
        packs_dir,
    });
    Ok(())
}

fn load_packs(dir: Option<&Path>, exe_dir: &Path) -> Catalog {
    let started = Instant::now();
    let catalog = match dir {
        Some(dir) => packs::load_dir(dir, Some(exe_dir)),
        None => Catalog {
            packs: Vec::new(),
            errors: vec![PackError {
                dir: exe_dir.join("packs"),
                message: "No packs folder was found next to Flick.".into(),
            }],
        },
    };
    for error in &catalog.errors {
        log(&format!("pack {}: {}", error.dir.display(), error.message));
    }
    log(&format!(
        "loaded {} pack(s) from {} in {:.1} ms",
        catalog.packs.len(),
        dir.map(|d| d.display().to_string()).unwrap_or_default(),
        started.elapsed().as_secs_f64() * 1000.0
    ));
    catalog
}

fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let settings = MenuItem::with_id(app, "settings", "Settings\u{2026}", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit Flick", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&settings, &quit])?;
    TrayIconBuilder::with_id("main")
        .icon(app.default_window_icon().cloned().expect("bundled icon"))
        .tooltip("Flick")
        .menu(&menu)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "settings" => open_settings(app),
            "quit" => app.exit(0),
            _ => {}
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

fn screen_centre() -> Placement {
    let (w, h) = unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) };
    let (x, y) = (w / 2, h / 2);
    Placement {
        x,
        y,
        scale: scale_at(POINT { x, y }),
    }
}

/// Six slices with realistic labels, for measurements independent of installed packs.
fn sample_content() -> WheelContent {
    let labels = [
        "Merge",
        "Compress all",
        "To images",
        "Copy path",
        "Zip",
        "Split",
    ];
    WheelContent {
        slices: labels
            .iter()
            .map(|label| Slice {
                id: format!("bench.{}", label.to_lowercase()),
                label: label.to_string(),
            })
            .collect(),
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
