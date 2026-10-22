# How to make a theme

A theme is one JSON file in the `themes` folder: a layout style and nine colours, no code, so a
downloaded theme is safe to use. Pick themes in Settings → Wheel look. Flick ships four:

| Theme | Style | Look |
|---|---|---|
| `orbit` (default) | orbit | Round buttons on a dotted orbit, labels outside, file count in the centre |
| `glass-pie` | pie | Dark disc cut into slices, icon and label inside each, accent arc on the edge |
| `slim-ring` | ring | Thin segmented ring, labels outside, large icon of the action in the centre |
| `fluent-light` | pie | Light Windows 11 look with the system blue accent |

## The file

```json
{
  "id": "orbit",
  "name": "Orbit",
  "style": "orbit",
  "colors": {
    "accent": "#ff8a4c",
    "on_accent": "#1a0f08",
    "surface": "#2a2c34",
    "slice": "#2a2c34",
    "centre": "#15161b",
    "text": "#e6e7ec",
    "muted": "#b9bbc4",
    "outline": "#ffffff1f",
    "shadow": "#00000073"
  }
}
```

- `id`: lower-case letters, digits and dashes; unique. A file with the id `orbit` replaces the
  built-in default.
- `style`: `orbit`, `pie` or `ring`.
- Colours are `#rgb`, `#rrggbb` or `#rrggbbaa` (the last two digits are opacity).

| Colour | Used for |
|---|---|
| `accent` | The slice or button under the cursor, the orbit's pointer line, the pie's edge arc |
| `on_accent` | Icon and label on the accent; pick one with at least 4.5:1 contrast |
| `surface` | Buttons, ring segments, the pie's disc |
| `slice` | Pie slices drawn on the disc (a faint white on dark themes) |
| `centre` | The centre circle, and the pills behind labels drawn outside the wheel |
| `text` | Icons and the hovered label |
| `muted` | Other labels and secondary text such as "2 PDFs" |
| `outline` | Thin edges and the orbit's dotted track |
| `shadow` | The soft shadow under the wheel |

A broken file is skipped and the reason shows in Settings; the wheel falls back to `orbit`.
`cargo test -p flick-core theme` checks that every bundled theme loads.

## Icons

Each slice names its icon in its pack manifest (`icon = "merge"`). The wheel draws icons natively
from the set in `core/src/icons.rs`: `merge`, `split`, `rotate`, `image-pdf`, `convert`, `shrink`,
`compress`, `copy`, `zip`, `pages`. Unknown names get a generic icon.
