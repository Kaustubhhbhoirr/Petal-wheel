//! The Settings window (`ui/settings.html`) and the commands it calls.
//!
//! The window is a WebView2 page created when opened and destroyed when closed, so it costs
//! nothing while Flick idles in the tray.

use std::path::PathBuf;
use std::sync::Arc;

use serde::Serialize;
use tauri::{AppHandle, Manager, State, WebviewUrl, WebviewWindowBuilder};

use crate::config::{self, Config, SliceConfig};
use crate::controller::Shared;
use crate::hotkey::{HotkeyManager, Shortcut};
use crate::log::{log, log_path};
use crate::packs::{PackError, CORE_VERSION};
use crate::theme::{Rgba, Style, ThemeError};

const LABEL: &str = "settings";

/// Managed Tauri state behind the commands.
pub struct AppState {
    pub shared: Arc<Shared>,
    pub hotkeys: Arc<HotkeyManager>,
    pub config_path: Option<PathBuf>,
    pub packs_dir: Option<PathBuf>,
    pub theme_errors: Vec<ThemeError>,
}

#[derive(Serialize)]
pub struct SettingsView {
    version: &'static str,
    shortcut: String,
    shortcut_active: bool,
    default_shortcut: String,
    slices: Vec<SliceView>,
    packs: Vec<PackView>,
    errors: Vec<PackError>,
    config_path: Option<PathBuf>,
    packs_dir: Option<PathBuf>,
    log_path: Option<PathBuf>,
    theme: String,
    themes: Vec<ThemeView>,
    theme_errors: Vec<ThemeError>,
}

#[derive(Serialize)]
struct ThemeView {
    id: String,
    name: String,
    style: Style,
    /// CSS colours for the picker's swatches.
    swatches: [String; 3],
}

fn css(color: Rgba) -> String {
    format!(
        "rgba({}, {}, {}, {:.3})",
        (color.r * 255.0).round(),
        (color.g * 255.0).round(),
        (color.b * 255.0).round(),
        color.a
    )
}

#[derive(Serialize)]
struct SliceView {
    id: String,
    label: String,
    pack: String,
    enabled: bool,
    /// e.g. "2+ files: pdf".
    applies: String,
}

#[derive(Serialize)]
struct PackView {
    id: String,
    name: String,
    version: String,
    license: String,
    dir: PathBuf,
    slices: usize,
}

#[tauri::command]
pub fn get_settings(state: State<'_, AppState>) -> SettingsView {
    let catalog = state.shared.catalog.read().unwrap();
    let config = state.shared.config.read().unwrap();
    let slices = catalog
        .ordered_slices(&config.slices)
        .into_iter()
        .map(|s| {
            let pack = catalog
                .packs
                .iter()
                .find(|p| p.slices.iter().any(|ps| ps.id == s.id))
                .map(|p| p.name.clone())
                .unwrap_or_default();
            let count = match (s.min_files, s.max_files) {
                (1, Some(1)) => "1 file".to_string(),
                (min, Some(max)) if min == max => format!("{min} files"),
                (min, Some(max)) => format!("{min}-{max} files"),
                (1, None) => "1+ files".to_string(),
                (min, None) => format!("{min}+ files"),
            };
            SliceView {
                id: s.id.clone(),
                label: s.label.clone(),
                pack,
                enabled: !config.slices.disabled.contains(&s.id),
                applies: format!("{count}: {}", s.applies_to.join(", ")),
            }
        })
        .collect();
    SettingsView {
        version: CORE_VERSION,
        shortcut: state
            .hotkeys
            .current()
            .unwrap_or_else(|| config.shortcut())
            .to_string(),
        shortcut_active: state.hotkeys.is_active(),
        default_shortcut: Shortcut::default().to_string(),
        slices,
        packs: catalog
            .packs
            .iter()
            .map(|p| PackView {
                id: p.id.clone(),
                name: p.name.clone(),
                version: p.version.clone(),
                license: p.license.clone(),
                dir: p.dir.clone(),
                slices: p.slices.len(),
            })
            .collect(),
        errors: catalog.errors.clone(),
        config_path: state.config_path.clone(),
        packs_dir: state.packs_dir.clone(),
        log_path: log_path(),
        theme: crate::theme::pick(&state.shared.themes, &config.theme)
            .id
            .clone(),
        themes: state
            .shared
            .themes
            .iter()
            .map(|t| ThemeView {
                id: t.id.clone(),
                name: t.name.clone(),
                style: t.style,
                swatches: [
                    css(t.colors.surface),
                    css(t.colors.accent),
                    css(t.colors.text),
                ],
            })
            .collect(),
        theme_errors: state.theme_errors.clone(),
    }
}

/// Checks a shortcut typed in the recorder. Returns its normalised spelling, or why it cannot
/// be used.
#[tauri::command]
pub fn check_shortcut(state: State<'_, AppState>, shortcut: String) -> Result<String, String> {
    let parsed: Shortcut = shortcut.parse().map_err(|e| format!("{e}"))?;
    state.hotkeys.probe(parsed).map_err(|e| e.to_string())?;
    Ok(parsed.to_string())
}

/// Applies and saves the settings. The shortcut is registered first; if that fails nothing is
/// changed and the reason is returned.
#[tauri::command]
pub fn save_settings(
    state: State<'_, AppState>,
    shortcut: String,
    order: Vec<String>,
    disabled: Vec<String>,
    theme: String,
) -> Result<(), String> {
    if !state.shared.themes.iter().any(|t| t.id == theme) {
        return Err(format!("The theme \"{theme}\" is not installed."));
    }
    let parsed: Shortcut = shortcut.parse().map_err(|e| format!("{e}"))?;
    state.hotkeys.set(parsed).map_err(|e| e.to_string())?;
    let config = Config {
        shortcut: parsed.to_string(),
        theme,
        slices: SliceConfig { order, disabled },
    };
    *state.shared.config.write().unwrap() = config.clone();
    let path = state
        .config_path
        .as_ref()
        .ok_or("There is no place to save settings (APPDATA is not set).")?;
    config::save(path, &config).map_err(|e| format!("Could not save settings ({e})."))?;
    log(&format!("settings saved, shortcut {parsed}"));
    Ok(())
}

/// Called while the shortcut recorder has focus, so the current combo can be typed there.
#[tauri::command]
pub fn pause_hotkey(state: State<'_, AppState>, paused: bool) -> Result<(), String> {
    if paused {
        state.hotkeys.pause();
        Ok(())
    } else {
        state.hotkeys.resume().map_err(|e| e.to_string())
    }
}

/// Closes the Settings window (its Close button).
#[tauri::command]
pub fn close_settings(app: AppHandle) {
    if let Some(window) = app.get_webview_window(LABEL) {
        let _ = window.close();
    }
}

/// Opens the Settings window, or brings it to the front if it is already open.
pub fn open_settings(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(LABEL) {
        let _ = window.unminimize();
        let _ = window.set_focus();
        return;
    }
    let result = WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("settings.html".into()))
        .title("Flick Settings")
        .inner_size(600.0, 800.0)
        .min_inner_size(480.0, 480.0)
        .center()
        .focused(true)
        .build();
    if let Err(e) = result {
        log(&format!("could not open Settings: {e}"));
    }
}
