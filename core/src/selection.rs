//! Reads the files selected in the foreground Explorer window or on the Desktop.
//!
//! Uses the Shell COM interfaces (`IShellWindows` -> `IShellBrowser` -> `IFolderView`) rather
//! than simulating Ctrl+C, which would clobber the clipboard. Windows 11 tabbed Explorer shows
//! several shell views inside one top-level window; only the active tab's view is read.
//!
//! Call [`foreground_window`] at the moment the hotkey is pressed, before the wheel takes focus,
//! then [`selected_paths`] with that window.

use std::path::PathBuf;

use windows::core::{Interface, BOOL, PWSTR};
use windows::Win32::Foundation::{HWND, LPARAM};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, IServiceProvider,
    CLSCTX_LOCAL_SERVER, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE,
};
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Shell::{
    IFolderView, IShellBrowser, IShellItemArray, IShellWindows, IWebBrowser2, SID_STopLevelBrowser,
    ShellWindows, CSIDL_DESKTOP, SIGDN_FILESYSPATH, SVGIO_SELECTION, SWC_DESKTOP,
    SWFO_NEEDDISPATCH,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumChildWindows, GetAncestor, GetClassNameW, GetForegroundWindow, IsWindowVisible, GA_ROOT,
};

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Selection {
    /// Selected items that are real files or folders, in Explorer's order.
    pub paths: Vec<PathBuf>,
    /// Selected items with no file system path (This PC, Recycle Bin, items inside a zip).
    pub skipped: usize,
    pub source: Source,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Explorer,
    Desktop,
    /// The foreground window is not Explorer or the Desktop, so nothing is selected.
    #[default]
    Other,
}

#[derive(Debug, thiserror::Error)]
pub enum SelectionError {
    #[error("Could not read the Explorer selection ({0}).")]
    Com(#[from] windows::core::Error),
}

/// The window that has focus now. Capture it when the hotkey fires.
pub fn foreground_window() -> HWND {
    unsafe { GetForegroundWindow() }
}

/// Returns what is selected in `window` if it is an Explorer window or the Desktop.
///
/// Runs on its own short-lived single-threaded apartment thread, so it is safe to call from any
/// thread, including one that already initialised COM differently.
pub fn selected_paths(window: HWND) -> Result<Selection, SelectionError> {
    // HWND holds a raw pointer, which is not Send; pass the plain value across.
    let raw = window.0 as isize;
    std::thread::spawn(move || read_on_sta(HWND(raw as *mut _)))
        .join()
        .unwrap_or_else(|_| Ok(Selection::default()))
}

fn read_on_sta(window: HWND) -> Result<Selection, SelectionError> {
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE).ok()?;
        let result = read(window);
        CoUninitialize();
        result
    }
}

unsafe fn read(window: HWND) -> Result<Selection, SelectionError> {
    let root = GetAncestor(window, GA_ROOT);
    let source = match class_name(root).as_str() {
        "CabinetWClass" | "ExploreWClass" => Source::Explorer,
        "Progman" | "WorkerW" => Source::Desktop,
        _ => return Ok(Selection::default()),
    };

    let windows: IShellWindows = CoCreateInstance(&ShellWindows, None, CLSCTX_LOCAL_SERVER)?;
    let browser = match source {
        Source::Explorer => explorer_browser(&windows, root)?,
        Source::Desktop => Some(desktop_browser(&windows)?),
        Source::Other => None,
    };
    let Some(browser) = browser else {
        return Ok(Selection {
            source,
            ..Default::default()
        });
    };

    let view: IFolderView = browser.QueryActiveShellView()?.cast()?;
    let mut selection = Selection {
        source,
        ..Default::default()
    };
    // With nothing selected, Items fails rather than returning an empty array.
    let Ok(items) = view.Items::<IShellItemArray>(SVGIO_SELECTION) else {
        return Ok(selection);
    };
    for index in 0..items.GetCount()? {
        let item = items.GetItemAt(index)?;
        match item.GetDisplayName(SIGDN_FILESYSPATH) {
            Ok(name) => selection.paths.push(PathBuf::from(take_string(name))),
            Err(_) => selection.skipped += 1,
        }
    }
    Ok(selection)
}

/// Finds the shell browser for the active tab of the Explorer window `root`.
unsafe fn explorer_browser(
    windows: &IShellWindows,
    root: HWND,
) -> windows::core::Result<Option<IShellBrowser>> {
    let active_tab = active_tab(root);
    let mut fallback = None;
    for index in 0..windows.Count()? {
        let Ok(dispatch) = windows.Item(&VARIANT::from(index)) else {
            continue;
        };
        let Ok(web) = dispatch.cast::<IWebBrowser2>() else {
            continue;
        };
        if web.HWND().map(|h| h.0) != Ok(root.0 as isize) {
            continue;
        }
        let Ok(browser) = top_level_browser(&web) else {
            continue;
        };
        let Ok(tab) = browser.GetWindow() else {
            continue;
        };
        if Some(tab) == active_tab {
            return Ok(Some(browser));
        }
        // Pre-tabs Explorer (Windows 10) has exactly one browser per window.
        fallback.get_or_insert(browser);
    }
    Ok(fallback)
}

unsafe fn desktop_browser(windows: &IShellWindows) -> windows::core::Result<IShellBrowser> {
    let desktop = VARIANT::from(CSIDL_DESKTOP as i32);
    let empty = VARIANT::default();
    let mut hwnd = 0i32;
    let dispatch =
        windows.FindWindowSW(&desktop, &empty, SWC_DESKTOP, &mut hwnd, SWFO_NEEDDISPATCH)?;
    top_level_browser(&dispatch.cast()?)
}

unsafe fn top_level_browser(web: &IWebBrowser2) -> windows::core::Result<IShellBrowser> {
    web.cast::<IServiceProvider>()?
        .QueryService::<IShellBrowser>(&SID_STopLevelBrowser)
}

/// On Windows 11 each tab is a `ShellTabWindowClass` child; only the active one is visible.
unsafe fn active_tab(root: HWND) -> Option<HWND> {
    unsafe extern "system" fn visit(child: HWND, found: LPARAM) -> BOOL {
        if class_name(child) == "ShellTabWindowClass" && IsWindowVisible(child).as_bool() {
            *(found.0 as *mut Option<HWND>) = Some(child);
            return false.into();
        }
        true.into()
    }
    let mut found: Option<HWND> = None;
    let _ = EnumChildWindows(
        Some(root),
        Some(visit),
        LPARAM(&mut found as *mut _ as isize),
    );
    found
}

unsafe fn class_name(window: HWND) -> String {
    let mut buf = [0u16; 64];
    let len = GetClassNameW(window, &mut buf);
    String::from_utf16_lossy(&buf[..len.max(0) as usize])
}

/// Copies a COM-allocated string and frees it.
unsafe fn take_string(text: PWSTR) -> String {
    let value = text.to_string().unwrap_or_default();
    CoTaskMemFree(Some(text.0 as *const _));
    value
}
