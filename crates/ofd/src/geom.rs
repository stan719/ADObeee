//! OFD geometry and its conversion to PDF space.
//!
//! OFD measures in millimetres with the origin at the top left and y growing downwards
//! (GB/T 33190-2016; the basic types are in §7.3). PDF measures in points (1/72 inch) with y
//! growing upwards. The model keeps OFD's numbers as they are; these helpers do the conversion.

/// Points per millimetre (72 / 25.4).
pub const PT_PER_MM: f64 = 72.0 / 25.4;

/// Millimetres to PDF points.
pub fn mm_to_pt(mm: f64) -> f64 {
    mm * PT_PER_MM
}

/// A box (`ST_Box`): top-left corner, width and height, in millimetres.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OfdBox {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl OfdBox {
    /// The size of a page with this physical box, in PDF points.
    pub fn size_pt(&self) -> (f64, f64) {
        (mm_to_pt(self.w), mm_to_pt(self.h))
    }

    /// The matrix from this page's OFD space (mm, y down, origin at the page box's top left
    /// corner) to the PDF page space of a page whose media box is `[0 0 w h]` in points.
    pub fn page_to_pdf(&self) -> Matrix {
        let k = PT_PER_MM;
        Matrix { a: k, b: 0.0, c: 0.0, d: -k, e: -self.x * k, f: (self.y + self.h) * k }
    }
}

/// An affine matrix in PDF's convention: a point `(x, y)` maps to
/// `(a·x + c·y + e, b·x + d·y + f)`. OFD's `ST_Array` CTM uses the same six numbers.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Matrix {
    pub a: f64,
    pub b: f64,
    pub c: f64,
    pub d: f64,
    pub e: f64,
    pub f: f64,
}

impl Matrix {
    pub const IDENTITY: Matrix = Matrix { a: 1.0, b: 0.0, c: 0.0, d: 1.0, e: 0.0, f: 0.0 };

    pub fn translate(x: f64, y: f64) -> Matrix {
        Matrix { e: x, f: y, ..Matrix::IDENTITY }
    }

    /// `self` applied first, then `next`.
    pub fn then(&self, next: &Matrix) -> Matrix {
        Matrix {
            a: self.a * next.a + self.b * next.c,
            b: self.a * next.b + self.b * next.d,
            c: self.c * next.a + self.d * next.c,
            d: self.c * next.b + self.d * next.d,
            e: self.e * next.a + self.f * next.c + next.e,
            f: self.e * next.b + self.f * next.d + next.f,
        }
    }

    pub fn apply(&self, x: f64, y: f64) -> (f64, f64) {
        (self.a * x + self.c * y + self.e, self.b * x + self.d * y + self.f)
    }

    /// The matrix that undoes this one, or `None` when it flattens the plane (or isn't
    /// finite).
    pub fn inverse(&self) -> Option<Matrix> {
        let det = self.a * self.d - self.b * self.c;
        if !det.is_finite() || det.abs() < 1e-12 {
            return None;
        }
        let (a, b, c, d) = (self.d / det, -self.b / det, -self.c / det, self.a / det);
        let m = Matrix { a, b, c, d, e: -(self.e * a + self.f * c), f: -(self.e * b + self.f * d) };
        [m.a, m.b, m.c, m.d, m.e, m.f].iter().all(|v| v.is_finite()).then_some(m)
    }
}
