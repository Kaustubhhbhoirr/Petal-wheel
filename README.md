# Flick: free, offline file tools in a radial menu for Windows.

Select a file, press a shortcut, flick to an action. No uploads, no ads, no subscriptions, no internet.

> **Status:** v0.1 spike. This proves the risky parts (hotkey, Explorer selection, wheel overlay,
> sandboxed workers) before features are built. Not ready for daily use yet.

## How it works

1. Select one or more files in Explorer or on the Desktop.
2. Hold **Ctrl + Alt + Space**. A wheel opens at the cursor with actions that fit the files.
3. Move toward a slice and release. The result is saved next to the original.

Originals are never modified. If `report.pdf` already has a `report.docx` next to it, the output becomes
`report (1).docx`.

## Why

Merging chapter notes, compressing a PDF for a portal upload limit, or turning photos into a PDF should
not need a website that wants your file, an ad view, or a subscription. Flick does it locally, in one
gesture, and runs well on a Core i3 with 4 GB of RAM.

## Architecture

```
Flick core (Rust + Tauri 2)        idle: ~0% CPU
  hotkey · Explorer selection · wheel overlay · tray · job supervisor
          | JSON over stdin/stdout
  worker processes, one per job, each in its own Windows Job Object
  (memory cap, CPU rate cap, wall-clock limit, kill-on-close)
```

A frozen or crashing conversion only kills its own worker. The core stays responsive.

| Path | What it is |
|---|---|
| `core/` | Tauri 2 tray app: hotkey, selection reader, wheel, supervisor |
| `workers/pdf-light/` | Native Rust PDF worker (merge; split and friends later) |
| `ui/` | Wheel overlay front end (WebView2 variant) |
| `docs/` | Architecture notes and spike results |
| `SPEC.md` | Full spec and roadmap |

## Building

Requirements: Windows 10 1809+ or Windows 11 (x64), Rust stable (MSVC toolchain), Visual Studio 2022
Build Tools with the C++ workload, and the WebView2 runtime (preinstalled on Windows 11).

```bash
cargo build --workspace
cargo test --workspace
cargo run -p flick-core
```

## Principles

- **Offline by default.** The core makes zero network calls. No telemetry.
- **Originals are sacred.** Output is always a new file, written to a temp name and renamed on success.
- **Never hang the PC.** Every job runs in a separate, resource-limited process.
- **Real text in, real text out.** Generated PDFs keep selectable text.

## License

[AGPL-3.0](LICENSE). Third-party components and their licenses are listed in
[THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md).
