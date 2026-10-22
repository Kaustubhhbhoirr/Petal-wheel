# Architecture

See SPEC.md section 6 for the full design. This file records decisions as they are made during the spike.

## Worker protocol

The core starts a worker, writes one JSON request to its stdin, closes stdin, and reads one JSON response
from stdout. Diagnostics go to stderr. Exit code 0 means the response was written.

Request:

```json
{ "op": "merge", "inputs": ["C:\a.pdf", "C:\b.pdf"], "output": "C:\a - merged.pdf.flick-tmp" }
```

Response:

```json
{ "ok": true, "output": "C:\a - merged.pdf.flick-tmp", "pages": 12 }
{ "ok": false, "error": { "code": "encrypted", "message": "b.pdf is password protected." } }
```

The worker writes only to the `output` path it was given. The core owns the final name (collision rule
`name (1).ext`) and renames the temp file only after a successful response.

## Supervisor (`core/src/supervisor.rs`)

1. Create a Job Object: job memory cap, `BELOW_NORMAL` priority, hard CPU rate cap,
   `DIE_ON_UNHANDLED_EXCEPTION` and `KILL_ON_JOB_CLOSE` (nothing survives the core).
2. Spawn the worker with `CREATE_SUSPENDED`, assign it to the job, then resume its thread, so it
   never runs a single instruction outside the limits.
3. Wait on the job's I/O completion port. `JOB_MEMORY_LIMIT` means too heavy; `ACTIVE_PROCESS_ZERO`
   means done. The wall-clock limit and Cancel are enforced by `TerminateJobObject`, which also kills
   any processes the worker started.
4. stdout is capped at 1 MiB and stderr keeps its last 8 KiB, so a chatty worker cannot grow the core.

Tested against `flick-misbehave` (memory bomb, infinite loop, child that spins, crash, garbage output).

## Explorer selection (`core/src/selection.rs`)

The window in front is captured when the hotkey fires. If its root is `CabinetWClass` (Explorer) the
matching `IShellWindows` entry is found by HWND; on Windows 11 several tabs share that HWND, so the
entry whose `IShellBrowser::GetWindow` equals the visible `ShellTabWindowClass` child wins. For
`Progman`/`WorkerW` the Desktop browser comes from `FindWindowSW(CSIDL_DESKTOP, SWC_DESKTOP)`. The
selection is read with `IFolderView::Items(SVGIO_SELECTION)`; items without a file system path are
counted as skipped. COM runs on a short-lived STA thread per read.

Probe: `cargo run -p flick-core --example selection_probe [-- --foreground 3]`.

## v0.5: packs, toast and Settings

- **Packs** (`core/src/packs.rs`, format in [packs.md](packs.md)). Every folder under `packs/` with
  a `pack.toml` is loaded and validated at startup (about 1 ms). Problems are reported per pack, never
  fatal. The wheel's slices come from the loaded packs, filtered by the selection and ordered by the
  user's config. A job runs the slice's worker with that pack's limits (`jobs::run_slice`): once
  with all files (`combine`) or once per file (`each`), each run in its own Job Object. Slices with
  `{n}` in their output template get an empty temp folder instead of a temp file; the core moves
  the files out to numbered names and removes them all again if anything fails
  (`jobs::run_slice_with` holds this logic and is tested without Windows).
- **Config** (`core/src/config.rs`): `%APPDATA%\Flick\config.toml` holds the shortcut, slice order and
  disabled slices. A missing or broken file falls back to defaults; saves go through a temp file.
- **Toast** (`core/src/toast.rs`): a native layered card, drawn with the same Direct2D code as the
  wheel (`core/src/gfx.rs`), because Windows notifications can be turned off system-wide. States:
  running (Cancel), done (Open, Show in folder), failed, and notice (one action button). It never
  takes focus; auto-hide pauses while hovered.
- **Settings** (`core/src/settings.rs`, `ui/settings.*`): a WebView2 window created on open and
  destroyed on close. The shortcut recorder checks for conflicts as you press keys; `HotkeyManager`
  registers the new combo before releasing the old one, and pauses the hotkey while the recorder has
  focus.

Gotcha found while testing: `ShowWindow`/`SetTimer` send messages back into a window procedure
synchronously, so procedures use `try_borrow_mut` on their thread-local state and leave re-entrant
messages to `DefWindowProcW`.

## Wheel themes

The native wheel draws one of three layouts (`core/src/overlay/native.rs`): **orbit** (default),
**pie** and **ring**, coloured by a theme from `themes/*.json` (`core/src/theme.rs`, format in
[themes.md](themes.md)). The controller attaches the chosen theme to each `WheelContent`, so a
change in Settings applies on the next open. Icons are stroke paths kept as SVG path data
(`core/src/icons.rs`), parsed once into Direct2D geometry and drawn with a transform. Labels drawn
outside the wheel sit on a pill in the centre colour so they stay readable on any background.
Shadows are stacked translucent circles, because the software render target has no blur effect.
