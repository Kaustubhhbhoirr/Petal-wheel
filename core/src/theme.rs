//! Wheel themes: JSON files with a layout style and colours, no code, so downloading one is safe
//! (SPEC 9). Bundled themes live in the `themes` folder; `orbit` is the default and is also built
//! in, so the wheel always has a theme even if the folder is missing.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const DEFAULT_THEME: &str = "orbit";

/// The bundled default, compiled in as a fallback.
const BUILT_IN: &str = include_str!("../../themes/orbit.json");

/// How the wheel is laid out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Style {
    /// Round buttons on a dotted orbit, labels outside, file count in the centre.
    Orbit,
    /// A solid disc cut into slices, icon and label inside each.
    Pie,
    /// A thin segmented ring, labels outside, a large icon of the action in the centre.
    Ring,
}

/// An sRGB colour with alpha, each channel 0..=1.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Rgba {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Rgba {
    /// Parses `#rgb`, `#rrggbb` or `#rrggbbaa`.
    pub fn parse(text: &str) -> Option<Self> {
        let hex = text.strip_prefix('#')?;
        let digits: Vec<u8> = hex
            .chars()
            .map(|c| c.to_digit(16).map(|d| d as u8))
            .collect::<Option<_>>()?;
        let channel = |hi: u8, lo: u8| (hi * 16 + lo) as f32 / 255.0;
        match digits.as_slice() {
            [r, g, b] => Some(Self {
                r: channel(*r, *r),
                g: channel(*g, *g),
                b: channel(*b, *b),
                a: 1.0,
            }),
            [r1, r2, g1, g2, b1, b2] => Some(Self {
                r: channel(*r1, *r2),
                g: channel(*g1, *g2),
                b: channel(*b1, *b2),
                a: 1.0,
            }),
            [r1, r2, g1, g2, b1, b2, a1, a2] => Some(Self {
                r: channel(*r1, *r2),
                g: channel(*g1, *g2),
                b: channel(*b1, *b2),
                a: channel(*a1, *a2),
            }),
            _ => None,
        }
    }
}

impl<'de> Deserialize<'de> for Rgba {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let text = String::deserialize(d)?;
        Rgba::parse(&text).ok_or_else(|| {
            serde::de::Error::custom(format!(
                "\"{text}\" is not a colour (use #rgb, #rrggbb or #rrggbbaa)"
            ))
        })
    }
}

/// Every colour a wheel uses. Each layout uses the ones it needs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Colors {
    /// The hovered slice or button.
    pub accent: Rgba,
    /// Icon and label colour on the accent.
    pub on_accent: Rgba,
    /// Slices, buttons and the disc behind them.
    pub surface: Rgba,
    /// Slice fill on top of the surface (pie), often a faint white or black.
    pub slice: Rgba,
    /// The centre circle.
    pub centre: Rgba,
    /// Icons and labels.
    pub text: Rgba,
    /// Secondary text, e.g. "2 PDFs" and labels of slices not hovered.
    pub muted: Rgba,
    /// Thin outlines and the orbit track.
    pub outline: Rgba,
    /// Drop shadow under the wheel.
    pub shadow: Rgba,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Theme {
    pub id: String,
    pub name: String,
    pub style: Style,
    pub colors: Colors,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ThemeError {
    pub file: PathBuf,
    pub message: String,
}

impl Theme {
    /// The built-in Orbit theme.
    pub fn built_in() -> Self {
        serde_json::from_str(BUILT_IN).expect("bundled orbit theme is valid")
    }
}

/// Loads every `*.json` in `dir`. The built-in default is always present (a file with the same id
/// replaces it). Problems are reported per file.
pub fn load_dir(dir: Option<&Path>) -> (Vec<Theme>, Vec<ThemeError>) {
    let mut themes = vec![Theme::built_in()];
    let mut errors = Vec::new();
    let Some(dir) = dir else {
        return (themes, errors);
    };
    let mut files: Vec<PathBuf> = match std::fs::read_dir(dir) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| {
                p.extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("json"))
            })
            .collect(),
        Err(e) => {
            errors.push(ThemeError {
                file: dir.to_path_buf(),
                message: format!("Could not read the themes folder ({e})."),
            });
            return (themes, errors);
        }
    };
    files.sort();
    for file in files {
        let parsed = std::fs::read_to_string(&file)
            .map_err(|e| format!("Could not read it ({e})."))
            .and_then(|text| {
                serde_json::from_str::<Theme>(&text).map_err(|e| format!("Not a valid theme: {e}"))
            })
            .and_then(|theme| {
                let ok = !theme.id.is_empty()
                    && theme
                        .id
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
                if ok {
                    Ok(theme)
                } else {
                    Err(format!(
                        "Theme id \"{}\" must use only lower-case letters, digits and dashes.",
                        theme.id
                    ))
                }
            });
        match parsed {
            Ok(theme) => {
                if let Some(slot) = themes.iter_mut().find(|t| t.id == theme.id) {
                    *slot = theme;
                } else {
                    themes.push(theme);
                }
            }
            Err(message) => errors.push(ThemeError { file, message }),
        }
    }
    (themes, errors)
}

/// The theme with `id`, or the default when it is not installed.
pub fn pick<'a>(themes: &'a [Theme], id: &str) -> &'a Theme {
    themes
        .iter()
        .find(|t| t.id == id)
        .or_else(|| themes.iter().find(|t| t.id == DEFAULT_THEME))
        .unwrap_or(&themes[0])
}

/// Where to look for themes: `FLICK_THEMES_DIR`, then a `themes` folder next to the executable
/// or in one of its ancestors (the repository root during development).
pub fn default_dir(exe_dir: &Path) -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("FLICK_THEMES_DIR") {
        return Some(PathBuf::from(dir));
    }
    exe_dir
        .ancestors()
        .map(|d| d.join("themes"))
        .find(|d| d.is_dir())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours_parse() {
        let c = Rgba::parse("#ff8a4c").unwrap();
        assert_eq!((c.r, c.a), (1.0, 1.0));
        assert!((c.g - 138.0 / 255.0).abs() < 1e-6);
        assert_eq!(
            Rgba::parse("#fff").unwrap(),
            Rgba::parse("#ffffff").unwrap()
        );
        assert!((Rgba::parse("#00000080").unwrap().a - 128.0 / 255.0).abs() < 1e-6);
        for bad in ["ff8a4c", "#ff8a4", "#gg0000", "", "#"] {
            assert!(Rgba::parse(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn built_in_theme_is_orbit() {
        let theme = Theme::built_in();
        assert_eq!(theme.id, DEFAULT_THEME);
        assert_eq!(theme.style, Style::Orbit);
    }

    #[test]
    fn bundled_themes_all_load() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../themes");
        let (themes, errors) = load_dir(Some(&dir));
        assert!(errors.is_empty(), "{errors:#?}");
        let ids: Vec<&str> = themes.iter().map(|t| t.id.as_str()).collect();
        for id in ["orbit", "glass-pie", "slim-ring", "fluent-light"] {
            assert!(ids.contains(&id), "missing {id} in {ids:?}");
        }
        assert_eq!(ids.iter().filter(|id| **id == "orbit").count(), 1);
    }

    #[test]
    fn bad_files_are_reported_and_skipped() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("broken.json"), "{ not json").unwrap();
        let mut bad_colour: serde_json::Value = serde_json::from_str(BUILT_IN).unwrap();
        bad_colour["id"] = "bad-colour".into();
        bad_colour["colors"]["accent"] = "orange".into();
        std::fs::write(dir.path().join("colour.json"), bad_colour.to_string()).unwrap();
        let mut bad_id: serde_json::Value = serde_json::from_str(BUILT_IN).unwrap();
        bad_id["id"] = "Bad Id".into();
        std::fs::write(dir.path().join("id.json"), bad_id.to_string()).unwrap();
        std::fs::write(dir.path().join("notes.txt"), "ignored").unwrap();

        let (themes, errors) = load_dir(Some(dir.path()));
        assert_eq!(themes.len(), 1);
        assert_eq!(errors.len(), 3, "{errors:#?}");
        assert!(errors.iter().any(|e| e.message.contains("not a colour")));
        assert!(errors.iter().any(|e| e.message.contains("lower-case")));
    }

    #[test]
    fn unknown_theme_falls_back_to_default() {
        let (themes, _) = load_dir(None);
        assert_eq!(pick(&themes, "does-not-exist").id, DEFAULT_THEME);
    }
}
