# v0.1 spike results

Measured 2026-10-22. Release builds (`opt-level = "s"`, LTO, stripped).

> **Not yet measured on an i3-class machine.** Everything below comes from the development laptop.
> The exit criteria in SPEC section 12 still need the same scripts run on a low-end PC (see
> [How to reproduce](#how-to-reproduce)).

**Machine:** Intel Core Ultra 9 275HX (24 threads), 15.4 GB RAM, Intel + RTX 5060 Laptop GPU,
Windows 11 build 26300, 1920×1080 at 100% scaling.

## Decision: native wheel renderer

The wheel is drawn natively with Direct2D (software render target) into a layered window.
WebView2 stays in the project for future windows such as Settings, but not for the wheel.

| Renderer | First open | Median open | p95 | Idle private memory | Idle processes |
|---|---|---|---|---|---|
| **Native (Direct2D, software target)** | **7.3 ms** | **3.1 ms** | 3.9 ms | **6.1 MB** (8.8 MB after one open) | 1 |
| Native (Direct2D, hardware target) | 23.7 ms | 3.4 ms | 3.7 ms | 53.3 MB | 1 |
| WebView2, cold (create on open, destroy on close) | 415 ms | 349 ms | 370 ms | 4.6 MB (6.3 MB after one open) | 1 |
| WebView2, warm (one hidden window kept alive) | 17 ms | 8.3 ms | 9.1 ms | 152 MB | 7 |

Idle CPU was **0.00%** for every variant over a 30 s window after a 15 s settle.

- Cold WebView2 misses the 100 ms budget by 3.5× on a fast machine, so it would be far worse on an i3.
- Warm WebView2 is fast but costs ~150 MB and six extra processes forever, which defeats the purpose
  of an always-running tray app on a 4 GB PC.
- Native is the fastest and close to the smallest. The hardware Direct2D target loaded the GPU
  driver into the process (~47 MB) for no visible benefit at this size, so the software target is used.

"Open" is the time from the controller asking for the wheel until its first frame is on screen
(for WebView2: until the page reports a painted frame).

## End to end: merge from Explorer

Three PDFs selected in Explorer, then hold the shortcut, flick up to Merge, release (driven by
`scripts/e2e-merge.ps1`, which sends real key and mouse input):

| Run | Wheel open (total) | of which reading the selection | Merge | Result |
|---|---|---|---|---|
| 1st after start | 44.4 ms | 35.8 ms | 592 ms | `Chapter 1 - merged.pdf` (4 pages) |
| 2nd | 20.4 ms | 17.6 ms | 173 ms | `Chapter 2 - merged.pdf` |
| 3rd (name taken) | 11.0 ms | 8.6 ms | 111 ms | `Chapter 1 - merged (1).pdf` |
| 4th, released in the centre | 12.6 ms | 9.5 ms | none | cancelled, nothing written |

Checked afterwards:

- Pages are in natural filename order (Chapter 1, 2, 10), and every page's text is still selectable.
- The original PDFs' SHA-256 hashes are unchanged.
- Reading the Explorer selection is now the largest part of open latency, about 36 ms on the first
  read after start (COM start-up) and about 9 to 18 ms after that. It is the first thing to optimise
  if an i3 misses the 100 ms budget, for example by keeping one COM thread alive instead of starting
  one per read.

## Findings

1. **The default shortcut is already taken on the development PC.** `Ctrl+Alt+Space` was registered
   by another app (the Claude desktop app's quick-entry shortcut or PowerToys are the likely owners).
   Flick detected the conflict and logged a clear message. A `--shortcut` option now exists, and the
   Settings window should offer to pick another combo on first run when the default is taken.
2. **Windows notifications can be switched off.** On the development PC they are off system-wide
   (`ToastEnabled = 0`), so the "Merge done" toast never appeared even though Flick sent it. The
   spec's toast (progress, Cancel, "Click to open", "Show in folder") should be Flick's own small
   native window, not a Windows notification.
3. **Hold-to-open works with real input.** Press, cursor tracking, slice highlight, release and
   centre-release cancel all behaved as designed. Click mode (tap to open, click a slice) is not built.
4. **WebView2 wheel shows a title bar** after Flick changes its window style to stop it taking focus.
   Cosmetic, and moot now that the native renderer was chosen.

## Still to do for the spike exit criteria

- [ ] Run the scripts below on an i3-class PC with 4 GB RAM, and on a throttled VM (2 cores, 4 GB).
- [ ] Windows 11 tabbed Explorer: select files in a second tab and confirm the active tab is read.
- [ ] Desktop: select files on the desktop and confirm they are read.
- [ ] Try the gesture by hand at 125% to 175% scaling and on a second monitor.
- [ ] Decide on click mode after trying it.

## How to reproduce

```powershell
cargo build --workspace --release

# Open latency, 20 opens per renderer (also saves screen captures of the wheel)
.\scripts\bench-capture.ps1 -Renderer native -OutDir $env:TEMP\flick-bench
.\scripts\bench-capture.ps1 -Renderer webview -OutDir $env:TEMP\flick-bench

# Idle CPU and memory of the whole process tree
.\scripts\measure-idle.ps1 -Renderer native -OpenOnce

# Real gesture against sample PDFs (moves the mouse and presses keys for about 3 s)
cargo run -p pdf-light --example make_samples -- $env:USERPROFILE\Documents\Flick-test
Start-Process .\target\release\flick.exe -ArgumentList '--shortcut', 'Ctrl+Alt+Shift+F12'
.\scripts\e2e-merge.ps1 -Folder $env:USERPROFILE\Documents\Flick-test `
    -Select 'Chapter 1.pdf','Chapter 2.pdf' -Mods 0x11,0x12,0x10 -Key 0x7B
```

Open latency in normal use is also logged to `%LOCALAPPDATA%\Flick\flick.log`.
