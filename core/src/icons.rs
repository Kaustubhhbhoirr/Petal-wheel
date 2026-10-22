//! The wheel's icon set: 24x24 stroke icons kept as SVG path data, parsed into simple segments
//! that the native renderer turns into Direct2D geometry.
//!
//! A slice names its icon in its pack manifest (`icon = "merge"`). A path such as
//! `icons/merge.svg` resolves by its file stem, and unknown names get a generic icon.

/// (name, SVG path data) in a 24x24 box, drawn with round 1.8 px strokes.
const ICONS: &[(&str, &str)] = &[
    ("merge", "M4 4h6v6H4z M14 4h6v6h-6z M7 10v2.5a2 2 0 0 0 2 2h6a2 2 0 0 0 2-2V10 M12 14.5V21 M9.5 18.5 12 21l2.5-2.5"),
    ("split", "M6 3h12v7H6z M6 14h12v7H6z M3 12h2 M8 12h2 M14 12h2 M19 12h2"),
    ("rotate", "M20 11a8 8 0 1 0-2.34 5.66 M20 4v7h-7"),
    ("image-pdf", "M6 3h9l4 4v14H6z M15 3v4h4 M8.5 17l2.5-2.5 2 2 3-3 M10.5 10.5h.01"),
    ("convert", "M4 8h13 M14 5l3 3-3 3 M20 16H7 M10 13l-3 3 3 3"),
    ("shrink", "M4 14h6v6 M20 10h-6V4 M14 10l7-7 M3 21l7-7"),
    ("compress", "M4 9h16 M4 15h16 M12 2.5v4 M10 4.5l2 2 2-2 M12 21.5v-4 M10 19.5l2-2 2 2"),
    ("copy", "M9 9h11v12H9z M5 15H4V3h11v1"),
    ("zip", "M6 3h12v18H6z M12 3v2 M12 7v2 M12 11v2 M10.5 14.5h3v3.5h-3z"),
    ("pages", "M3 5h18v14H3z M3 16l5-5 4 4 3-3 6 6 M16 8.5h.01"),
    ("more", "M4.5 12h1 M11.5 12h1 M18.5 12h1"),
    ("rotate-left", "M4 11a8 8 0 1 1 2.34 5.66 M4 4v7h7"),
    ("rotate-180", "M8 20V9a4 4 0 0 1 8 0v11 M13 17l3 3 3-3"),
    ("remove-pages", "M6 3h9l4 4v14H6z M15 3v4h4 M9.5 14h6"),
    ("organize", "M4 4h7v7H4z M13 4h7v7h-7z M4 13h7v7H4z M13 13h7v7h-7z"),
    ("target", "M12 3a9 9 0 1 0 .01 0z M12 8a4 4 0 1 0 .01 0z M12 11.5h.01"),
];

/// Used when a slice has no icon or names one Flick does not have.
const FALLBACK: &str = "M12 3a9 9 0 1 0 .01 0z M12 8v8 M8 12h8";

/// The path data for an icon name, accepting `merge`, `icons/merge.svg` or `MERGE`.
pub fn path_for(name: Option<&str>) -> &'static str {
    let stem = name
        .map(|n| {
            let file = n.rsplit(['/', '\\']).next().unwrap_or(n);
            file.strip_suffix(".svg")
                .unwrap_or(file)
                .to_ascii_lowercase()
        })
        .unwrap_or_default();
    ICONS
        .iter()
        .find(|(n, _)| *n == stem)
        .map(|(_, d)| *d)
        .unwrap_or(FALLBACK)
}

/// One absolute drawing step.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Segment {
    Move(f32, f32),
    Line(f32, f32),
    Cubic([f32; 6]),
    Arc {
        rx: f32,
        ry: f32,
        rotation: f32,
        large: bool,
        sweep: bool,
        to: (f32, f32),
    },
    Close,
}

/// Parses SVG path data (M L H V C A Z, absolute and relative, with implicit repeats).
pub fn parse(data: &str) -> Result<Vec<Segment>, String> {
    let mut tokens = Tokens {
        s: data.as_bytes(),
        i: 0,
    };
    let mut out = Vec::new();
    let (mut x, mut y) = (0f32, 0f32);
    let (mut start_x, mut start_y) = (0f32, 0f32);
    let mut command = None;
    loop {
        tokens.skip_separators();
        let Some(&c) = tokens.s.get(tokens.i) else {
            break;
        };
        if c.is_ascii_alphabetic() {
            command = Some(c);
            tokens.i += 1;
        }
        let cmd = command.ok_or("path data must start with a command")?;
        let rel = cmd.is_ascii_lowercase();
        let (ox, oy) = if rel { (x, y) } else { (0.0, 0.0) };
        match cmd.to_ascii_uppercase() {
            b'M' => {
                x = ox + tokens.number()?;
                y = oy + tokens.number()?;
                start_x = x;
                start_y = y;
                out.push(Segment::Move(x, y));
                // Further pairs after a move are lines.
                command = Some(if rel { b'l' } else { b'L' });
            }
            b'L' => {
                x = ox + tokens.number()?;
                y = oy + tokens.number()?;
                out.push(Segment::Line(x, y));
            }
            b'H' => {
                x = ox + tokens.number()?;
                out.push(Segment::Line(x, y));
            }
            b'V' => {
                y = oy + tokens.number()?;
                out.push(Segment::Line(x, y));
            }
            b'C' => {
                let mut p = [0f32; 6];
                for (i, v) in p.iter_mut().enumerate() {
                    *v = tokens.number()? + if i % 2 == 0 { ox } else { oy };
                }
                x = p[4];
                y = p[5];
                out.push(Segment::Cubic(p));
            }
            b'A' => {
                let rx = tokens.number()?;
                let ry = tokens.number()?;
                let rotation = tokens.number()?;
                let large = tokens.flag()?;
                let sweep = tokens.flag()?;
                x = ox + tokens.number()?;
                y = oy + tokens.number()?;
                out.push(Segment::Arc {
                    rx,
                    ry,
                    rotation,
                    large,
                    sweep,
                    to: (x, y),
                });
            }
            b'Z' => {
                out.push(Segment::Close);
                x = start_x;
                y = start_y;
                command = None;
            }
            other => return Err(format!("unsupported path command '{}'", other as char)),
        }
    }
    Ok(out)
}

struct Tokens<'a> {
    s: &'a [u8],
    i: usize,
}

impl Tokens<'_> {
    fn skip_separators(&mut self) {
        while self
            .s
            .get(self.i)
            .is_some_and(|c| c.is_ascii_whitespace() || *c == b',')
        {
            self.i += 1;
        }
    }

    fn number(&mut self) -> Result<f32, String> {
        self.skip_separators();
        let start = self.i;
        if matches!(self.s.get(self.i), Some(b'-' | b'+')) {
            self.i += 1;
        }
        let mut dot = false;
        while let Some(&c) = self.s.get(self.i) {
            if c.is_ascii_digit() {
                self.i += 1;
            } else if c == b'.' && !dot {
                dot = true;
                self.i += 1;
            } else {
                break;
            }
        }
        std::str::from_utf8(&self.s[start..self.i])
            .ok()
            .and_then(|t| t.parse().ok())
            .ok_or_else(|| format!("expected a number at byte {start}"))
    }

    /// Arc flags are single digits and may be written without separators ("a2 2 0 011 1").
    fn flag(&mut self) -> Result<bool, String> {
        self.skip_separators();
        match self.s.get(self.i) {
            Some(b'0') => {
                self.i += 1;
                Ok(false)
            }
            Some(b'1') => {
                self.i += 1;
                Ok(true)
            }
            _ => Err(format!("expected an arc flag at byte {}", self.i)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_parses() {
        for (name, data) in ICONS.iter().chain([&("fallback", FALLBACK)]) {
            let segments = parse(data).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert!(matches!(segments[0], Segment::Move(..)), "{name}");
        }
    }

    #[test]
    fn relative_commands_and_implicit_lines() {
        let s = parse("M4 4h6v6H4z m10 0 l2 2 3 3").unwrap();
        assert_eq!(
            s,
            [
                Segment::Move(4.0, 4.0),
                Segment::Line(10.0, 4.0),
                Segment::Line(10.0, 10.0),
                Segment::Line(4.0, 10.0),
                Segment::Close,
                Segment::Move(14.0, 4.0),
                Segment::Line(16.0, 6.0),
                Segment::Line(19.0, 9.0),
            ]
        );
    }

    #[test]
    fn arcs_with_packed_numbers() {
        let s = parse("M7 10v2.5a2 2 0 0 0 2 2").unwrap();
        assert_eq!(
            s[2],
            Segment::Arc {
                rx: 2.0,
                ry: 2.0,
                rotation: 0.0,
                large: false,
                sweep: false,
                to: (9.0, 14.5)
            }
        );
        let s = parse("M20 11a8 8 0 1 0-2.34 5.66").unwrap();
        assert!(matches!(
            s[1],
            Segment::Arc {
                large: true,
                sweep: false,
                ..
            }
        ));
    }

    #[test]
    fn bad_data_is_an_error() {
        assert!(parse("4 4").is_err());
        assert!(parse("M4").is_err());
        assert!(parse("M4 4 Q1 1 2 2").is_err());
    }

    #[test]
    fn names_resolve() {
        assert_eq!(path_for(Some("merge")), path_for(Some("icons/merge.svg")));
        assert_eq!(path_for(Some("MERGE")), path_for(Some("merge")));
        assert_eq!(path_for(Some("unknown")), FALLBACK);
        assert_eq!(path_for(None), FALLBACK);
    }
}
