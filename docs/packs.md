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
icon = "icons/merge.svg"      # optional, not drawn yet
applies_to = ["pdf"]          # extensions, or ["*"] for any file
min_files = 2                 # default 1
# max_files = 10              # default: no limit
output = "{stem} - merged.pdf"
action = { worker = "pdf-light", op = "merge" }
```

A slice appears on the wheel only when **every** selected file has one of its extensions and the
number of files is within `min_files`..`max_files`.

### Output names

`output` is a file name template. `{stem}` is the first input's name without its extension and
`{ext}` is its extension; inputs are sorted by name first (natural order, so "Chapter 2" comes before
"Chapter 10"). The result is always saved next to that first input and never replaces a file: if the
name is taken, Flick uses `name (1).ext`, `name (2).ext`, and so on. Folders in the template are
rejected.

## The worker protocol

Flick starts the worker once per job inside a Windows Job Object with the pack's limits, writes one
JSON request to its stdin and closes it, then reads one JSON response from stdout:

```json
{ "op": "merge", "inputs": ["C:\\notes\\Chapter 1.pdf", "C:\\notes\\Chapter 2.pdf"],
  "output": "C:\\notes\\~Chapter 1 - merged.pdf.1234-0.flick-tmp" }
```

```json
{ "ok": true, "output": "C:\\notes\\~Chapter 1 - merged.pdf.1234-0.flick-tmp", "pages": 3 }
{ "ok": false, "error": { "code": "encrypted", "message": "Chapter 2.pdf is password protected." } }
```

Rules for workers:

- Write only to `output`, and refuse if it already exists. Flick renames it to the real name after a
  successful response, and deletes it after a failure.
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
