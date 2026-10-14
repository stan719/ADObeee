//! OFD's attribute value syntax: numbers, arrays, boxes, `DeltaX`/`DeltaY` with `g` repeats, and
//! `AbbreviatedData` path commands. Every function returns an error for input it can't read.

use crate::Limits;
use crate::geom::{Matrix, OfdBox};
use crate::model::PathCmd;

/// Coordinates and sizes beyond this (a kilometre in millimetres times a thousand) are refused,
/// so later arithmetic stays finite.
const MAX_ABS: f64 = 1.0e9;

pub(crate) fn number(s: &str) -> Result<f64, String> {
    let v: f64 = s.trim().parse().map_err(|_| format!("{s:?} is not a number"))?;
    if !v.is_finite() || v.abs() > MAX_ABS {
        return Err(format!("{s:?} is out of range"));
    }
    Ok(v)
}

/// A whitespace-separated list of numbers (`ST_Array`), at most `limits.max_numbers` of them.
pub(crate) fn numbers(s: &str, limits: &Limits) -> Result<Vec<f64>, String> {
    let mut out = Vec::new();
    for tok in s.split_whitespace() {
        if out.len() >= limits.max_numbers {
            return Err(format!("more than {} numbers in one value", limits.max_numbers));
        }
        out.push(number(tok)?);
    }
    Ok(out)
}

/// `ST_Box`: "x y w h", with a width and height that aren't negative.
pub(crate) fn ofd_box(s: &str, limits: &Limits) -> Result<OfdBox, String> {
    match *numbers(s, limits)?.as_slice() {
        [x, y, w, h] if w >= 0.0 && h >= 0.0 => Ok(OfdBox { x, y, w, h }),
        [_, _, _, _] => Err(format!("box {s:?} has a negative size")),
        _ => Err(format!("box {s:?} needs four numbers (x y width height)")),
    }
}

/// An object's `Boundary`: like [`ofd_box`], but a negative width or height is made positive
/// (the corner stays), and says so.
pub(crate) fn boundary(s: &str, limits: &Limits) -> Result<(OfdBox, bool), String> {
    match *numbers(s, limits)?.as_slice() {
        [x, y, w, h] => Ok((OfdBox { x, y, w: w.abs(), h: h.abs() }, w < 0.0 || h < 0.0)),
        _ => Err(format!("box {s:?} needs four numbers (x y width height)")),
    }
}

/// A CTM: six numbers.
pub(crate) fn matrix(s: &str, limits: &Limits) -> Result<Matrix, String> {
    match *numbers(s, limits)?.as_slice() {
        [a, b, c, d, e, f] => Ok(Matrix { a, b, c, d, e, f }),
        _ => Err(format!("matrix {s:?} needs six numbers")),
    }
}

/// A colour value: numbers, or `#xx` hexadecimal components.
pub(crate) fn color_value(s: &str, limits: &Limits) -> Result<Vec<f64>, String> {
    let mut out = Vec::new();
    for tok in s.split_whitespace() {
        if out.len() >= limits.max_numbers {
            return Err(format!("more than {} numbers in one value", limits.max_numbers));
        }
        let v = match tok.strip_prefix('#') {
            Some(hex) => f64::from(u8::from_str_radix(hex, 16).map_err(|_| format!("{tok:?} is not a hexadecimal component"))?),
            None => number(tok)?,
        };
        out.push(v);
    }
    Ok(out)
}

/// `DeltaX`/`DeltaY`: numbers where `g N v` stands for `v` repeated `N` times. Only the first
/// `needed` steps are kept (a text of n characters has n − 1), so a huge `N` costs nothing.
pub(crate) fn deltas(s: &str, needed: usize, limits: &Limits) -> Result<Vec<f64>, String> {
    let needed = needed.min(limits.max_numbers);
    let mut out = Vec::new();
    let mut toks = s.split_whitespace();
    while let Some(tok) = toks.next() {
        if tok == "g" {
            let n = toks.next().ok_or("`g` needs a count and a value")?;
            let v = toks.next().ok_or("`g` needs a count and a value")?;
            let n: u64 = n.parse().map_err(|_| format!("`g` count {n:?} is not a whole number"))?;
            let v = number(v)?;
            let room = needed.saturating_sub(out.len());
            let take = usize::try_from(n).unwrap_or(usize::MAX).min(room);
            out.extend(std::iter::repeat_n(v, take));
        } else {
            let v = number(tok)?;
            if out.len() < needed {
                out.push(v);
            }
        }
    }
    Ok(out)
}

/// `AbbreviatedData`: `S`/`M` x y, `L` x y, `Q` x1 y1 x y, `B` x1 y1 x2 y2 x y,
/// `A` rx ry angle large sweep x y, `C` (close). Numbers after a command without a new letter
/// repeat it. `Z` is read as `C`.
pub(crate) fn path(s: &str, limits: &Limits) -> Result<Vec<PathCmd>, String> {
    let mut spaced = String::with_capacity(s.len() + 16);
    for ch in s.chars() {
        if matches!(ch, 'S' | 'M' | 'L' | 'Q' | 'B' | 'A' | 'C' | 'Z') {
            spaced.push(' ');
            spaced.push(ch);
            spaced.push(' ');
        } else if ch == ',' {
            spaced.push(' ');
        } else {
            spaced.push(ch);
        }
    }
    let toks: Vec<&str> = spaced.split_whitespace().collect();
    if toks.len() > limits.max_numbers {
        return Err(format!("more than {} numbers in one path", limits.max_numbers));
    }
    let mut out = Vec::new();
    let mut i = 0;
    let mut cmd: Option<char> = None;
    while let Some(&tok) = toks.get(i) {
        let letter = tok.chars().next().filter(|c| c.is_ascii_uppercase() && tok.len() == 1 && *c != 'E');
        if let Some(c) = letter {
            i += 1;
            if c == 'C' || c == 'Z' {
                out.push(PathCmd::Close);
                cmd = None;
                continue;
            }
            cmd = Some(c);
        }
        let c = cmd.ok_or_else(|| format!("path data starts with {tok:?}, not a command"))?;
        let arity = match c {
            'S' | 'M' | 'L' => 2,
            'Q' => 4,
            'B' => 6,
            'A' => 7,
            other => return Err(format!("unknown path command {other:?}")),
        };
        let args = toks.get(i..i + arity).ok_or_else(|| format!("path command {c} needs {arity} numbers"))?;
        let mut v = [0.0; 7];
        for (slot, a) in v.iter_mut().zip(args) {
            *slot = number(a)?;
        }
        i += arity;
        out.push(match c {
            'S' | 'M' => PathCmd::Move(v[0], v[1]),
            'L' => PathCmd::Line(v[0], v[1]),
            'Q' => PathCmd::Quad(v[0], v[1], v[2], v[3]),
            'B' => PathCmd::Cubic(v[0], v[1], v[2], v[3], v[4], v[5]),
            _ => PathCmd::Arc { rx: v[0], ry: v[1], angle: v[2], large: v[3] != 0.0, sweep: v[4] != 0.0, x: v[5], y: v[6] },
        });
    }
    Ok(out)
}
