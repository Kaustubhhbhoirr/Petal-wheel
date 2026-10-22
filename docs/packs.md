# How to make a pack

A pack is a folder with a `pack.toml` manifest and a worker program. Each pack adds actions
("slices") to the wheel. Flick loads every pack in its `packs` folder at startup; a pack with a
problem is skipped and the reason is shown in Settings → Packs, so it never stops the others.

## Folder layout

```
packs/
  my-pack/
    pack.toml
    bin/my-worker.exe
```

## pack.toml

```toml
id = "pdf-light"              # lower-case letters, digits and dashes; unique
name = "PDF Light"
version = "0.5.0"             # semver
requires_core = ">=0.5"       # semver range the Flick version must match
requires_packs = []           # other packs whose workers your slices use
license = "AGPL-3.0"

[worker]
kind = "native"               # only "native" for now; "python" comes with the Python runtime
entry = "bin/pdf-light.exe"   # must stay inside the pack folder

[limits]                      # optional; these are the defaults
memory_mb = 512               # 64-4096
time_seconds = 120            # 5-3600
cpu_percent = 50              # 5-100, share of the whole machine

[[slices]]
id = "merge"                  # becomes "pdf-light.merge"
label = "Merge"
icon = "merge"                # optional; one of the names below
applies_to = ["pdf"]          # extensions, or ["*"] for any file
min_files = 2                 # default 1
# max_files = 10              # default: no limit
mode = "combine"              # "combine" (default) or "each"
output = "{stem} - merged.pdf"
action = { worker = "pdf-light", op = "merge" }
```

A slice appears on the wheel only when **every** selected file has one of its extensions and the
number of files is within `min_files`..`max_files`.

### Icons

The wheel draws slice icons itself, so `icon` is a name, not a file: `merge`, `split`, `rotate`,
`image-pdf`, `convert`, `shrink`, `compress`, `copy`, `zip` or `pages`. Any other name stops the pack
from loading, so a typo shows up in Settings → Packs. Without an icon the slice shows its label only.

### Modes

- `mode = "combine"` (the default): the worker runs **once with every selected file** and makes one
  result, named from the first file. Merge and images to PDF work like this.
- `mode = "each"`: the worker runs **once per selected file**, each run in its own Job Object with the
  pack's limits, and each result is saved next to its own input. Split, rotate and the image
  conversions work like this. If one file fails, the others are still saved and the toast says which
  one failed and why. Cancel stops the job before the next file; files already saved stay, since they
  are complete.

### Output names

`output` is a file name template. `{stem}` is the input's name without its extension and `{ext}` is
its extension. In `combine` mode that is the first input: inputs are sorted by name first (natural
order, so "Chapter 2" comes before "Chapter 10"). The result is always saved next to that input and
never replaces a file: if the name is taken, Flick uses `name (1).ext`, `name (2).ext`, and so on.
Folders in the template are rejected.

### Several files from one run

A template with `{n}` makes each run produce **several numbered files**, for example one per page:

```toml
[[slices]]
id = "split"
label = "Split"
icon = "split"
applies_to = ["pdf"]
mode = "each"                     # each selected PDF is split on its own
output = "{stem} - page {n}.pdf"  # Report - page 01.pdf ... Report - page 12.pdf
action = { worker = "pdf-light", op = "split" }
```

Instead of one output file the worker gets an empty temp folder (`output_dir`, see below), writes its
files there and lists them in order. Flick then moves each one to its name, where `{n}` is its 1-based
position zero-padded to the width of the count (`01` to `12`, but `1` to `9` for nine files), with the
usual `(1)` rule for names that are taken. If the worker fails, or any file cannot be moved, the files
already moved are removed again and the temp folder is deleted: a run saves all of its files or none.
`{n}` works in both modes.

## The worker protocol

Flick starts the worker once per run (once per job, or once per file in `each` mode) inside a
Windows Job Object with the pack's limits, writes one JSON request to its stdin and closes it, then
reads one JSON response from stdout:

```json
{ "op": "merge", "inputs": ["C:\\notes\\Chapter 1.pdf", "C:\\notes\\Chapter 2.pdf"],
  "output": "C:\\notes\\~Chapter 1 - merged.pdf.1234-0.flick-tmp" }
```

```json
{ "ok": true, "output": "C:\\notes\\~Chapter 1 - merged.pdf.1234-0.flick-tmp", "pages": 3 }
{ "ok": false, "error": { "code": "encrypted", "message": "Chapter 2.pdf is password protected." } }
```

In `each` mode `inputs` holds one file per request. For a slice whose `output` has `{n}`, the request
has `output_dir` (an existing, empty folder) instead of `output`, and a successful response lists the
files written there, in order:

```json
{ "op": "split", "inputs": ["C:\\notes\\Report.pdf"],
  "output_dir": "C:\\notes\\~Report - page n.pdf.1234-1.flick-tmp" }
```

```json
{ "ok": true, "pages": 12,
  "outputs": ["C:\\notes\\~Report - page n.pdf.1234-1.flick-tmp\\1.pdf", "..."] }
```

Other fields of a successful response, such as `pages`, are optional.

Rules for workers:

- Write only to `output` (or inside `output_dir`), and refuse to replace a file that already exists.
  Flick renames results to their real names after a successful response, and deletes them after a
  failure. Only files directly inside `output_dir` are moved, whatever paths the response lists.
- Never modify the inputs.
- Put a clear, user-facing sentence in `error.message`; it is shown in the toast.
- Diagnostics go to stderr. Exit with code 0 once the response is written.

If the worker goes over its memory or time limit, or the user presses Cancel, Flick ends the whole
job (including any processes the worker started) and the original files stay untouched.

## Trying a pack during development

While developing, the pack folder can hold just `pack.toml`: if `entry` is missing inside the pack,
Flick uses a binary with the same file name next to `flick.exe` (for example
`target\release\pdf-light.exe`). Set `FLICK_PACKS_DIR` to load packs from another folder.
`cargo test -p flick-core --test packs` checks that the packs in this repository load cleanly.
