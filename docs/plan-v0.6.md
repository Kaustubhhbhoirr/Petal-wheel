# v0.6 plan: nested wheels, action tiers, the Flick app window, a better shortcut

Status: in progress. Each step below is one or more commits; this file is updated as steps land.

## Decisions made for the owner

| Question | Decision | Why |
|---|---|---|
| Default wheel style | **Orbit** stays the default and gets nesting first (owner's choice). Pie and Ring show the same child fan until they get their own nested look. | Owner picked design B; nesting is built behind one interface so other styles plug in. |
| Name of the new tier field | `mode = "quick" \| "choice" \| "app"`, as the brief asks. The existing run-count field becomes `runs = "once" \| "each"`. | Avoids two meanings for `mode`. Pre-1.0, only bundled packs used the old field; they move to `requires_core = ">=0.6"`. |
| Releasing over a parent slice | Runs the parent's `default` child if the manifest names one, otherwise does nothing (same as cancel). | Fast for the common case (Rotate: 90° right) without surprising anyone when no default makes sense. |
| More ring | Built by the core, not the manifest: if more than 6 top-level actions fit the selection, the 6th slot becomes **More** and holds the rest. | Packs stay simple; order stays stable because it comes from config + pack order, never usage. |
| Thumbnails | A sandboxed `pdf-render` worker using Windows' built-in `Windows.Data.Pdf` renderer, kept alive per open document, rendering on demand into a capped cache. | Offline, nothing new to license, and no PDF renderer in the always-running core. |
| Default shortcut | See [shortcut.md](shortcut.md). | Researched against Windows, PowerToys, Claude desktop and common apps. |

## Steps

1. Slice tree: tiers, children (2 levels; data model allows 3), More ring, stable order, tests.
2. Nested Orbit wheel: hover delay opens a child fan on an outer orbit; hub, Esc or right-click closes
   one level; releasing in the hub cancels; wheel clamped into the screen so fans never go off-edge.
3. Worker ops: rotate by 90/-90/180 (all or chosen pages), organize/remove pages, split by ranges,
   compress presets, compress to a target size with an honest result. Tests.
4. Rotate and Compress as `choice` slices; compress toast shows before and after sizes.
5. Flick app window (created on demand, destroyed on close): thumbnails, Remove pages, Rotate pages,
   Compress to target size; Organize and Split by ranges if time allows, otherwise marked stubs.
6. Shortcut: research, new default, recorder with conflict warnings and suggestions, mouse-button
   hold, opt-in modifier-only hold.
7. Measurements (wheel open, sub-ring open, app window open, RAM with a 200-page PDF), manual test
   checklist, docs.
