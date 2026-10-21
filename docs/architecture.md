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
