# Flick: free, offline file tools in a radial menu for Windows.

Select a file, press a shortcut, flick to an action. No uploads, no ads, no subscriptions, no internet.

> **Status:** v0.5 alpha in progress. Packs, a Settings window and Flick's own progress toast work
> on Windows 11, with the actions listed below. Spike measurements are in
> [docs/spike-results.md](docs/spike-results.md). Not ready for daily use yet.

## How it works

1. Select one or more files in Explorer or on the Desktop.
2. Hold **Ctrl + Alt + Space** (you can change it in Settings). A wheel opens at the cursor with actions that fit the files.
3. Move toward a slice and release. The result is saved next to the original.
4. A small card in the corner shows progress (with Cancel), then **Open** and **Show in folder**.

Settings (tray icon → Settings…) lets you record a new shortcut, reorder or turn off actions, and see
installed packs. They are saved in a plain file, `%APPDATA%\Flick\config.toml`.

Originals are never modified. If `report.pdf` already has a `report.docx` next to it, the output becomes
`report (1).docx`.

## Actions

| Action | Works on | Result, saved next to the original |
|---|---|---|
| **Merge** | 2 or more PDFs | `Chapter 1 - merged.pdf`, in name order (Chapter 2 before Chapter 10) |
| **Split** | PDFs | One file per page: `Report - page 01.pdf` ... `Report - page 12.pdf` |
| **Rotate** | PDFs | `Report - rotated.pdf`, every page turned 90° clockwise |
| **To PDF** | JPG and PNG images | `photo 1 - images.pdf`, a page per image sized to it; JPGs keep their exact quality |
| **To PNG** | JPG, WebP, BMP | `photo.png`, transparency kept |
| **To JPG** | PNG, WebP, BMP | `photo.jpg` (quality 90), transparent areas white |
| **Shrink** | JPG, PNG, WebP, BMP | `photo - small.jpg`, at most 1600 px on the longest side, or a message if it would not get smaller |

With several files selected, Split, Rotate and the image conversions handle each file on its own; if
one file fails, the others are still saved. PDF tools keep text selectable. Password-protected PDFs
are refused with a clear message.

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
| `core/` | Tauri 2 tray app: hotkey, selection reader, wheel, toast, pack loader, supervisor |
| `packs/` | Pack manifests (`pack.toml`); each pack adds wheel actions and names its worker |
| `workers/pdf-light/` | Native Rust PDF worker: merge, split, rotate |
| `workers/image-tools/` | Native Rust image worker: images to PDF, PNG/JPG conversion, shrink |
| `ui/` | Settings window, plus the WebView2 wheel kept from the renderer comparison |
| `docs/` | Architecture, [how to make a pack](docs/packs.md), spike results |
| `scripts/` | Measurement and end-to-end test scripts (PowerShell) |
| `SPEC.md` | Full spec and roadmap |

## Building

Requirements: Windows 10 1809+ or Windows 11 (x64), Rust stable (MSVC toolchain), Visual Studio 2022
Build Tools with the C++ workload, and the WebView2 runtime (preinstalled on Windows 11).

```bash
cargo build --workspace
cargo test --workspace
cargo run -p flick-core
```

If another app already owns the shortcut, Flick shows a toast with a **Change shortcut** button that
opens Settings. Problems are also logged to `%LOCALAPPDATA%\Flick\flick.log`. For a one-off run with a
different shortcut (not saved):

```bash
cargo run -p flick-core -- --shortcut "Ctrl+Alt+Shift+F12"
```

## Principles

- **Offline by default.** The core makes zero network calls. No telemetry.
- **Originals are sacred.** Output is always a new file, written to a temp name and renamed on success.
- **Never hang the PC.** Every job runs in a separate, resource-limited process.
- **Real text in, real text out.** Generated PDFs keep selectable text.

## License

[AGPL-3.0](LICENSE). Third-party components and their licenses are listed in
[THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md).

## Acknowledgements

Built by Kaustubh Bhoir, with help from Claude (Anthropic) as a coding assistant.
