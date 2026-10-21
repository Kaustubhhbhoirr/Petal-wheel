# Flick: Spec and Roadmap

> **Flick: free, offline file tools in a radial menu for Windows.**
> Select a file, press a shortcut, flick to an action. No uploads, no ads, no subscriptions, no internet.

Status: planning. This document is the single source of truth until v0.1 ships. Items marked **[assumed]** are defaults chosen so work can start; change them freely.

---

## 0. Builder brief (read this first, then say "start building")

**What we are building:** Flick, a free, open-source, offline Windows tray app. The user selects files in Explorer, holds Ctrl + Alt + Space, and a radial wheel opens at the cursor with actions that fit the file type (PDF merge, split, compress, to Word, images to PDF, and so on). Releasing over a slice runs it. Output is saved next to the original and never overwrites it. With nothing selected, the wheel becomes a launcher (pinned apps, system tools, recent files).

**Owner:** Kaustubh Bhoir, Computer Engineering student, India. This is a portfolio and resume project, so code quality, a clean README and GitHub-readiness matter. He develops on a fast machine but the app must run well on low-end i3/i5 PCs with 4 GB RAM.

**Hard rules for the builder:**
1. Never hang the PC: every worker runs in its own Windows Job Object with memory, CPU and time limits.
2. Never overwrite or modify an original file. Use the naming rule `name (1).ext`.
3. Generated PDFs must keep real selectable text (no flattened images).
4. Fully offline. No network calls except the optional pack downloader, which verifies SHA-256.
5. License is AGPL-3.0. Keep a `LICENSE`, `THIRD-PARTY-NOTICES.md` and credits for every dependency.
6. Windows-specific code (hotkey, Explorer selection, Job Objects, overlay) can only be tested on real Windows. In a Linux sandbox, build and test only the worker logic and the cross-platform parts, and say clearly which parts are untested.

**First task: the v0.1 spike (section 12).** Build in this order, committing after each step:
1. Repo scaffold: Tauri 2 + Rust workspace (`core/`, `workers/pdf-light/`, `docs/`), CI that builds on Windows, README with the tagline.
2. Native `pdf-light` worker with a `merge` op, driven by JSON on stdin/stdout, with unit tests on sample PDFs (including encrypted and zero-page cases).
3. Job Object wrapper in the core, tested with a deliberately misbehaving worker (memory bomb, infinite loop). The core must kill it and report a clear message.
4. Global hotkey with hold and release detection.
5. Explorer selection reader (`IShellWindows`), including tabbed Explorer and Desktop.
6. Wheel overlay at the cursor. Build both a WebView2 version and a native version, then measure open latency, idle CPU and idle RAM to decide the renderer.
7. Wire it together: select two PDFs, open the wheel, choose Merge, get `merged.pdf` next to the originals plus a toast.

**Done means:** the exit criteria in section 12 are met and the measurements are written down in `docs/spike-results.md`.

**Do not build yet:** the pack manager, themes, launcher mode, OCR, Office conversion. They come after the spike.

---

## 1. Why Flick exists

Everyday PDF and file jobs (merge six chapter notes into one file, compress for a portal upload limit, convert PDF to Word, turn PNGs into a PDF) are blocked by websites that want an upload, a size limit, an ad, or a subscription. Local desktop tools exist, but they are separate apps: open the app, browse for the file, find the tool.

Flick's difference is the workflow: **select the file, press a key, pick the action, done.** The result lands next to the original.

### Goals
- Handle the 90% of daily file jobs with one gesture, fully offline.
- Never hang or slow a low-end PC (target machine: Core i3, 4 GB RAM, HDD).
- Free forever, open source, open contribution.
- Small core, optional packs: users install only what they need.
- Customizable: slices, themes, shortcut.

### Non-goals (for now)
- A full PDF editor. Page organize / remove / rotate yes; text editing no.
- Corrupted-file repair, PDF/A, translation, document comparison.
- Cloud features, accounts, telemetry, auto-upload of anything.
- macOS and Linux in v1 (later; see roadmap).

---

## 2. Principles

1. **Offline by default.** The core makes zero network calls. The only network use is downloading packs, and only when the user clicks.
2. **Originals are sacred.** Output is always a new file; never overwrite. Name collisions become `name (1).ext`.
3. **Never hang the PC.** The always-running core does no heavy work. Every job runs in a separate, resource-limited process that is killed if it misbehaves.
4. **Simple first.** If a feature needs a manual, it is wrong for v1.
5. **Real text in, real text out.** Any PDF Flick generates from text sources (for example Markdown to PDF) must keep real selectable text, not rasterized pages.

---

## 3. Decisions

| Topic | Decision | Notes |
|---|---|---|
| Name | **Flick** | See section 13 for clash risks and what to verify before launch. |
| Repo name | `flick-wheel` | Keeps search results distinct from other things called Flick. |
| License | **AGPL-3.0** | Needed for best PDF to Word quality (PyMuPDF is AGPL). Verify every dependency license before release. |
| Core | **Rust + Tauri 2** | Small footprint; contributors can add tools without touching it. |
| Workers | Python (shared embedded runtime) + native Rust for light PDF ops | Mature conversion libraries live in Python. |
| Platform v1 | **Windows 10 (1809+) and Windows 11, x64** [confirmed] | WebView2 runtime required on Win10 (installer bootstraps it). |
| Wheel trigger | **Hold the shortcut to open, move toward a slice, release to run** [your call, default kept] | Optional "click mode": press once to open, click a slice. |
| Default shortcut | **Ctrl + Alt + Space** [confirmed, must be customisable] | Configurable. Conflicts are detected and reported. |
| Distribution | Installer + portable `.exe`, GitHub Releases, then winget / Scoop / Chocolatey | |

---

## 4. User experience

### 4.1 With a file selected
1. User selects one or more files in Explorer (or on the Desktop).
2. Presses the shortcut. The wheel fades in at the cursor in under 100 ms.
3. Slices depend on what is selected (single type, mixed types, many files).
4. Release over a slice to run. Esc or release in the center cancels.
5. A small toast shows progress, then "Done. Click to open" and a "Show in folder" action. A Cancel button is always available while a job runs.

### 4.2 Output rule
- Written next to the original, same folder.
- Same base name, new extension: `report.pdf` becomes `report.docx`.
- Collision: `report (1).docx`, `report (2).docx`.
- Multi-file results (merge, images to PDF) are named from the first file (`Chapter 1 - merged.pdf`), sorted by filename.

### 4.3 Wheel contents (v1)

| Selection | Slices |
|---|---|
| One PDF | To Word, To images, To text, Remove pages, Organize pages, Split, Compress, Password, Watermark |
| Several PDFs | Merge, Compress all, To images |
| One image | PNG / JPG / WebP convert, To PDF, Shrink |
| Several images | Combine into one PDF, Convert all, Shrink all |
| Word / Excel (v2) | To PDF (Excel also to CSV) |
| Markdown (v3) | To PDF, Preview |
| Any file | Copy path, Zip |

More than 8 slices: the wheel shows the top 6 to 8 and a **More** slice that opens an outer ring. Slice order is user-configurable.

### 4.4 With nothing selected: launcher mode
The wheel becomes a launcher: pinned apps and folders, system shortcuts (Settings, Calculator, Task Manager, system info), and recent files. Scrolling the mouse wheel flips between pages of slices.

### 4.5 Page tools
- **Remove pages:** a small prompt such as `3, 5-7`.
- **Organize pages:** thumbnail grid. Click to delete, drag to reorder, rotate, then Save as new file.
- **Split:** by range, every N pages, or one file per page.

---

## 5. Feature map and packs

Features ship in **packs** so a PDF-only user never downloads Dev tools.

| Pack | Contents | Version |
|---|---|---|
| **Core** (always) | Tray, hotkey, wheel, themes, settings, pack manager, launcher mode | v1 |
| **PDF Light** | Merge, split, remove pages, organize, rotate, extract, password, watermark, page numbers | v1 |
| **PDF Convert** | PDF to Word, to images, to text; images to PDF | v1 |
| **PDF Compress** | Compress; compress to target size | v1 |
| **Image** | PNG / JPG / WebP conversion, resize, shrink | v1 |
| **Office** | Word / Excel / PowerPoint to PDF (needs Microsoft Office or LibreOffice installed) | v2 |
| **OCR** | Scanned PDF to searchable PDF or Word | v2 (optional) |
| **Dev** | Markdown to PDF / HTML, preview, JSON / CSV / YAML convert, base64, SHA256, batch rename, QR code | v3 |

### OCR notes
Offline OCR is possible (Windows built-in OCR engine, or Tesseract). Both are good on clean printed English and poor on handwriting and math formulas. Test on real scanned files before promising it. Ships as an optional pack.

### Student-friendly extras (candidates, post-v1)
Merge question papers; N-up printing (2 or 4 pages per sheet); strip blank pages from scans.

---

## 6. Architecture

```
+-------------------------------+
| Flick core (Rust + Tauri)     |   idle: ~0% CPU, small RAM
|  - hotkey listener            |
|  - Explorer selection reader  |
|  - wheel overlay window       |
|  - tray icon + settings       |
|  - job queue + supervisor     |
+---------------+---------------+
                | JSON messages (stdin/stdout or named pipe)
      +---------+---------+
      |                   |
 native worker       Python worker(s)
 (merge/split)       (pdf2docx, PyMuPDF, Pillow ...)
 each in its own Windows Job Object
```

### 6.1 Core responsibilities (and only these)
- Listen for the hotkey, event-driven, **no polling**.
- Read the current Explorer selection.
- Draw the wheel and report which slice was chosen.
- Start, monitor and cancel worker processes; show toasts.

### 6.2 Hotkey and hold-to-open
`RegisterHotKey` reports key press but not release. Hold-to-open needs a low-level keyboard hook (`WH_KEYBOARD_LL`) or a short `GetAsyncKeyState` check only while the wheel is open. The hook must return quickly; the core does no work inside it.

### 6.3 Reading the Explorer selection
Use the Shell COM interfaces (`IShellWindows` to find the foreground Explorer window, then the shell view's selected items). Fall back to the Desktop view. **Spike item:** confirm behavior with Windows 11 tabbed Explorer and with the Desktop. Do not simulate Ctrl+C as a default; it is intrusive.

### 6.4 Never hang the PC
- **One process per job.** A crashing or frozen conversion only kills its own worker; the wheel stays alive.
- **Windows Job Objects** enforce per-worker limits: memory cap, below-normal priority, CPU rate limit, wall-clock time limit, and `KILL_ON_JOB_CLOSE` so nothing is orphaned.
- On limit breach: kill the worker, show "This file was too heavy for the safe limit", leave the original untouched, and delete any partial output.
- **Queue:** one heavy job at a time on weak machines; others wait. Cancel is always available.
- **Low-end mode (auto):** detected from CPU core count, RAM and GPU. Turns off blur and animations, lowers worker limits.
- Output is written to a temp file and renamed on success, so a crash never leaves a half-written file with a real name.

### 6.5 Performance budgets (to be measured, not assumed)
| Metric | Target |
|---|---|
| Wheel appears after shortcut | < 100 ms on an i3 |
| Core idle CPU | ~0% |
| Core idle RAM | < 30 MB (see risk below) |
| Merging two 10 MB PDFs | a few seconds on an i3 |
| Install size (core only) | small; measure and publish |

**Risk: WebView2 memory.** A Tauri window uses WebView2, which adds processes and memory when a window exists. Options to evaluate in the v0 spike: (a) create the wheel window lazily on hotkey and destroy it after, keeping the idle core webview-free; (b) draw the wheel natively (Direct2D through the Rust `windows` crate) for a guaranteed light overlay. Pick based on measured idle RAM and open latency on a throttled VM and a real older laptop.

### 6.6 Python workers
- One shared embedded Python runtime; packs add only their own libraries (no per-pack Python copies).
- Common light operations (merge, split, remove, rotate, extract) may be implemented natively in Rust to avoid starting Python at all.
- Suggested libraries: `pdf2docx` + PyMuPDF (PDF to Word, PDF to images, text), `pikepdf` (passwords, structure, compress), `pypdf` (fallbacks), Pillow (images), `img2pdf` (images to PDF without recompression).
- **Compression:** start with pikepdf + image recompression via Pillow; evaluate Ghostscript for stronger results (it is AGPL, which fits, but adds size).

---

## 7. Packs

A pack is a folder with a manifest, its worker, and its libraries.

```toml
# pack.toml
id = "pdf-light"
name = "PDF Light"
version = "1.0.0"
requires_core = ">=1.0"
requires_packs = []            # e.g. ["pdf-light"] for Dev's Markdown to PDF
license = "AGPL-3.0"

[worker]
kind = "native"                # or "python"
entry = "bin/pdf-light.exe"

[limits]
memory_mb = 512
time_seconds = 120

[[slices]]
id = "merge"
label = "Merge"
icon = "icons/merge.svg"
applies_to = ["pdf"]
min_files = 2
action = { worker = "pdf-light", op = "merge" }
```

### Pack manager
- Installer checkboxes choose first-run packs.
- Settings > Packs lists installed and available packs; install, update, remove.
- Packs download from GitHub Releases with **SHA-256 verification**; a pack that fails the hash is rejected.
- A **full offline installer** (core + all official packs) is published for users with no internet.
- Removing a pack removes its slices cleanly. Version mismatches with the core are detected and explained.

### Contributing a pack
Anyone can publish a community pack. Unofficial packs show a clear "community pack" label and a warning before first run, since packs run code.

---

## 8. Slices and customization

- Every slice is a small config entry: name, icon, applicable file types, min/max files, action.
- Users can add **custom slices**: open a folder, launch an app, run a script. Script slices show a warning on creation.
- Slice order, enabled slices, and the shortcut are editable in a simple settings window; config lives in a plain file so it can be backed up or shared.

---

## 9. Themes

- A theme is a **JSON file plus images, no code**, so downloading one is safe.
- Controls: colors, wheel size, ring thickness, blur strength, animation speed, fonts, icon set.
- Bundled presets: **Glass**, **Mac-style**, **Windows 11**, **Windows 7 Aero**, **GNOME-style**, **Minimal dark**. These are "inspired by" looks with original icons and artwork; do not copy Apple or Microsoft assets or logos.
- Glass is the GPU-heavy one, which is why low-end mode drops it automatically.
- Suggested logo: a ring with a soft inner glow.

```json
{
  "name": "Glass",
  "wheel": { "radius": 140, "ring": 56, "blur": 24, "animationMs": 120 },
  "colors": { "bg": "#14141acc", "slice": "#ffffff14", "hover": "#7cc4ff", "text": "#f2f2f2" },
  "fonts": { "label": "Segoe UI Variable" },
  "lowEndFallback": "minimal-dark"
}
```

---

## 10. Security and privacy

- Core: no network calls, no telemetry, no analytics.
- Packs: hash-verified downloads, a clear permissions summary, community-pack warnings.
- Workers run with limited resources and, where possible, restricted file access (only the selected files and the output folder).
- Original files are never modified; temp files are cleaned up on exit and on crash recovery.
- Password and watermark tools never store passwords.
- Windows SmartScreen will warn on unsigned builds. Investigate free code signing for open source projects before v1.0 (for example the SignPath Foundation program) and document the checksum for each release.

---

## 11. Licensing and credits

- Flick is **AGPL-3.0**. Source stays public; forks must stay open.
- Ship `LICENSE`, a `THIRD-PARTY-NOTICES.md` listing every library with its license and copyright holder, and an **About / Credits** screen in the app (PyMuPDF / Artifex, pdf2docx, pikepdf, Pillow, and so on).
- Before each release, re-verify every dependency's license and that its terms are compatible with AGPL distribution.
- Trademark and name use: see section 13.

---

## 12. Roadmap

### v0.1: Spike (prove the risky parts)
Goal: answer the unknowns before building features.
- Global hotkey with hold-to-open and release detection.
- Read the Explorer selection (tabbed Explorer, Desktop).
- Wheel overlay opens at the cursor; compare WebView2 versus native drawing for idle RAM and open latency.
- One real action end to end: merge two PDFs (native) with the output naming rule and a toast.
- Job Object limits working on a deliberately misbehaving test worker (memory bomb, infinite loop).
- **Exit criteria:** measured numbers on an i3-class machine for open latency, idle CPU and idle RAM; a decision on the wheel renderer.

### v0.5: Alpha
- Pack system and manifest loader; PDF Light and Image packs.
- Type-aware slices; multi-select behavior; More ring.
- Settings window (shortcut, slice order, themes basic).
- Low-end mode auto-detection.

### v1.0: Public release
- PDF Light, PDF Convert, PDF Compress and Image packs (the 10 core jobs: PDF to Word, PDF to images, images to PDF, merge, split, remove pages, organize, compress, password add / remove, watermark).
- Launcher mode (no selection).
- Themes: all six presets.
- Installer, portable exe, offline installer, checksums, code signing plan done.
- Docs: README, "How to add a slice", "How to make a pack", "How to make a theme".
- **Exit criteria:** works on Windows 10 and 11; budgets met on a low-end machine; no data loss in testing (originals untouched in every failure case).

### v2: Power
- Office pack (Word / Excel / PowerPoint to PDF), OCR pack (optional), PDF to Excel for clean tables.
- Pack Manager polish; community pack listing.

### v3: Developers and reach
- Dev pack (Markdown to PDF and preview, JSON / CSV / YAML, base64, hashes, batch rename, QR).
- Markdown to PDF can use the WebView2 print-to-PDF path, which keeps real selectable text.
- Linux port (hotkey and selection detection are the platform-specific parts); macOS later.

---

## 13. Name check and risks

**Decision:** the project is called **Flick**; the repo is `flick-wheel`.

Findings from web searches (unverified against live registries):
- A macOS app called Flick is listed on Homebrew as a radial command wheel triggered by holding a key and flicking the mouse, with no installs recorded in the past year.
- A Flick file-sharing app (getflick.io) is on the Microsoft Store.
- A Flick offline canvas-notes app for Windows exists on Product Hunt.
- Other uses: an AI filmmaking startup, a social media marketing platform, an Android launcher, Ruby and Flutter packages, a Python macOS app switcher.
- Several software trademark filings exist under FLICK (for example a screen-sharing filing by Flick Inc.).

**Before the public launch, verify and decide:**
1. Search the name directly on GitHub, the Microsoft Store, winget, Scoop, npm and PyPI, and check `.app` / `.dev` / `.io` domains.
2. Run a trademark search (USPTO and the Indian registry). This is not legal advice; consult a professional if the project grows.
3. Message the developers of the closest Windows and macOS Flick apps; a friendly note avoids confusion.
4. Always use the full tagline ("Flick: free, offline file tools in a radial menu for Windows") in the repo description, README first line, release notes and posts.
5. Renaming a repo before launch is cheap. Decide before tagging v1.0; after that, a rename costs users and search ranking.

---

## 14. Discoverability

- GitHub topics: `pdf-tools`, `radial-menu`, `offline`, `windows`, `open-source`, `file-converter`, `pdf-merge`.
- First line of the README is the tagline; repeat the same phrase in every listing.
- Package managers: winget, Scoop, Chocolatey.
- Listings and launches: AlternativeTo, Product Hunt, Show HN, relevant subreddits.
- A one-page site with a short demo GIF of the wheel (the wheel is the thing people will remember).

---

## 15. Suggested repo layout

```
flick-wheel/
  core/                 Rust + Tauri core (hotkey, selection, wheel, supervisor)
  packs/
    pdf-light/
    pdf-convert/
    pdf-compress/
    image/
  themes/               JSON themes + assets
  docs/                 slices, packs, themes, architecture
  installer/            Inno Setup / NSIS scripts
  LICENSE
  THIRD-PARTY-NOTICES.md
  SPEC.md
  README.md
```

---

## 16. Testing

- Unit tests for each worker op with sample PDFs and images (including big files, encrypted PDFs, zero-page edge cases).
- Failure tests: memory bomb, infinite loop, disk full, read-only folder, file in use, locked Explorer window. Every case must leave the original untouched and show a clear message.
- Performance tests on a throttled VM (2 cores, 4 GB RAM) and at least one real older laptop.
- Manual UX checks on Windows 10 and 11, single and multi-monitor, 100% to 175% display scaling.

---

## 17. Open questions

1. **Windows 10 support:** DECIDED, both 10 and 11. Assumed yes (1809+). Drop it if it complicates WebView2 or the installer.
2. **Hold versus click:** DECIDED, left to Claude: hold-to-open default, click mode optional; confirm after trying both in the spike.
3. **Default shortcut:** DECIDED, Ctrl + Alt + Space, user-customisable; check conflicts with PowerToys and common apps.
4. **Wheel renderer:** WebView2 or native drawing (decided by v0.1 measurements).
5. **Code signing:** which free option, if any, is available for the project.
6. **Rust learning curve:** DECIDED, left to Claude: scaffold the core, explain as we go. Original question: how much of the core you want to write yourself versus scaffold.
