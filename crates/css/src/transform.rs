//! 4 by 4 column-major matrix composition for transforms (spec 0010, AC-19).
//! Index is `row + 4 * column`, so `m[3]`, `m[7]`, `m[11]` and `m[15]` are
//! the bottom row, the one perspective writes.

use base::Size;

use crate::{Length, Transform, TransformFn};

impl Transform {
    /// The 4 by 4 column-major matrix for this transform on a border box of
    /// `size`, or `None` when the box has no transform at all.
    ///
    /// CSS Transforms 2, section 6.1: move to `transform-origin`, then
    /// `translate`, `rotate`, `scale` and each `transform` function, read
    /// left to right as one product, then move back (spec 0010, AC-19).
    /// The result maps box-local coords to box-local coords; callers place
    /// it in parent space by translating by the box's border box corner
    /// (spec 0010, AC-20). A pure translation is snapped to whole pixels so
    /// text stays crisp.
    pub fn matrix(&self, size: Size) -> Option<[f32; 16]> {
        if !self.has_transform() {
            return None;
        }
        let basis = |length: &Length, along: base::Au| length.resolve(Some(along)).to_f32_px();
        let origin = (
            basis(&self.origin.0, size.width),
            basis(&self.origin.1, size.height),
        );
        // `mat4_mul(a, b)` is "a, then b": compose from the point's side,
        // innermost first.
        let mut m = mat4_translate(-origin.0, -origin.1, 0.0);
        for f in self.transform.iter().rev() {
            m = mat4_mul(&m, &transform_fn_matrix(f, size));
        }
        if let Some((x, y)) = &self.scale {
            m = mat4_mul(&m, &mat4_scale(*x, y.unwrap_or(*x), 1.0));
        }
        if let Some(angle) = &self.rotate {
            m = mat4_mul(&m, &mat4_rotate_z(angle.degrees().to_radians()));
        }
        if let Some((x, y)) = &self.translate {
            let y = y.as_ref().map_or(0.0, |y| basis(y, size.height));
            m = mat4_mul(&m, &mat4_translate(basis(x, size.width), y, 0.0));
        }
        m = mat4_mul(&m, &mat4_translate(origin.0, origin.1, 0.0));
        if let Some(t) = mat4_pure_translation(&m) {
            return Some(mat4_translate(t[0].round(), t[1].round(), t[2].round()));
        }
        Some(m)
    }

    /// The product of the `transform` functions alone, on a border box of
    /// `size`, with no origin and none of the separate properties: the
    /// matrix `getComputedStyle` resolves `transform` to (CSSOM, spec 0010,
    /// AC-31). `None` for `transform: none`.
    pub fn functions_matrix(&self, size: Size) -> Option<[f32; 16]> {
        if self.transform.is_empty() {
            return None;
        }
        let mut m = mat4_identity();
        for f in self.transform.iter().rev() {
            m = mat4_mul(&m, &transform_fn_matrix(f, size));
        }
        Some(m)
    }

    /// The projection this box's `perspective` applies to its children, in
    /// its own border box space of `size`, about `perspective-origin`
    /// (initially the center), or `None` without a `perspective` (spec
    /// 0010, AC-19).
    pub fn perspective_matrix(&self, size: Size) -> Option<[f32; 16]> {
        let distance = self.perspective.as_ref()?.resolve(None).to_f32_px();
        if distance <= 0.0 {
            return None;
        }
        let (x, y) = self
            .perspective_origin
            .clone()
            .unwrap_or((Length::Percent(50.0), Length::Percent(50.0)));
        let origin = (
            x.resolve(Some(size.width)).to_f32_px(),
            y.resolve(Some(size.height)).to_f32_px(),
        );
        let m = mat4_mul(
            &mat4_translate(-origin.0, -origin.1, 0.0),
            &mat4_perspective(distance),
        );
        Some(mat4_mul(&m, &mat4_translate(origin.0, origin.1, 0.0)))
    }
}

/// A length in px, percentages of `along`.
fn px(length: &Length, along: base::Au) -> f32 {
    length.resolve(Some(along)).to_f32_px()
}

/// Multiply two column-major 4 by 4 matrices: `a` then `b` (`b * a`).
fn mat4_mul(a: &[f32; 16], b: &[f32; 16]) -> [f32; 16] {
    let mut out = [0.0; 16];
    for col in 0..4 {
        for row in 0..4 {
            let mut sum = 0.0;
            for k in 0..4 {
                sum += a[k + 4 * col] * b[row + 4 * k];
            }
            out[row + 4 * col] = sum;
        }
    }
    out
}

fn mat4_identity() -> [f32; 16] {
    let mut m = [0.0; 16];
    m[0] = 1.0;
    m[5] = 1.0;
    m[10] = 1.0;
    m[15] = 1.0;
    m
}

fn mat4_translate(x: f32, y: f32, z: f32) -> [f32; 16] {
    let mut m = mat4_identity();
    m[12] = x;
    m[13] = y;
    m[14] = z;
    m
}

fn mat4_scale(x: f32, y: f32, z: f32) -> [f32; 16] {
    let mut m = [0.0; 16];
    m[0] = x;
    m[5] = y;
    m[10] = z;
    m[15] = 1.0;
    m
}

fn mat4_rotate_z(angle: f32) -> [f32; 16] {
    let (s, c) = angle.sin_cos();
    let mut m = mat4_identity();
    m[0] = c;
    m[1] = s;
    m[4] = -s;
    m[5] = c;
    m
}

/// `rotateX(a)`, which is `rotate3d(1, 0, 0, a)`: y goes toward z.
fn mat4_rotate_x(angle: f32) -> [f32; 16] {
    let (s, c) = angle.sin_cos();
    let mut m = mat4_identity();
    m[5] = c;
    m[6] = s;
    m[9] = -s;
    m[10] = c;
    m
}

/// `rotateY(a)`, which is `rotate3d(0, 1, 0, a)`: z goes toward x.
fn mat4_rotate_y(angle: f32) -> [f32; 16] {
    let (s, c) = angle.sin_cos();
    let mut m = mat4_identity();
    m[0] = c;
    m[2] = -s;
    m[8] = s;
    m[10] = c;
    m
}

/// `perspective(d)`: `w = 1 - z / d`, the CSS `m34` entry.
fn mat4_perspective(d: f32) -> [f32; 16] {
    let mut m = mat4_identity();
    m[11] = -1.0 / d;
    m
}

/// The matrix for one transform function on a border box of `size`, which
/// translation percentages refer to (spec 0010, AC-19).
pub(crate) fn transform_fn_matrix(f: &TransformFn, size: Size) -> [f32; 16] {
    let (w, h) = (size.width, size.height);
    match f {
        TransformFn::Translate(x, y) => {
            let y = y.as_ref().map_or(0.0, |y| px(y, h));
            mat4_translate(px(x, w), y, 0.0)
        }
        TransformFn::TranslateX(x) => mat4_translate(px(x, w), 0.0, 0.0),
        TransformFn::TranslateY(y) => mat4_translate(0.0, px(y, h), 0.0),
        TransformFn::Translate3d(x, y, z) => {
            mat4_translate(px(x, w), px(y, h), px(z, base::Au::ZERO))
        }
        TransformFn::TranslateZ(z) => mat4_translate(0.0, 0.0, px(z, base::Au::ZERO)),
        TransformFn::Scale(x, y) => mat4_scale(*x, y.unwrap_or(*x), 1.0),
        TransformFn::ScaleX(x) => mat4_scale(*x, 1.0, 1.0),
        TransformFn::ScaleY(y) => mat4_scale(1.0, *y, 1.0),
        TransformFn::Scale3d(x, y, z) => mat4_scale(*x, *y, *z),
        TransformFn::ScaleZ(z) => mat4_scale(1.0, 1.0, *z),
        TransformFn::Rotate(a) => mat4_rotate_z(a.degrees().to_radians()),
        TransformFn::RotateX(a) => mat4_rotate_x(a.degrees().to_radians()),
        TransformFn::RotateY(a) => mat4_rotate_y(a.degrees().to_radians()),
        TransformFn::RotateZ(a) => mat4_rotate_z(a.degrees().to_radians()),
        TransformFn::Rotate3d(x, y, z, a) => {
            let (x, y, z) = (*x, *y, *z);
            let angle = a.degrees().to_radians();
            let (s, c) = angle.sin_cos();
            let len = (x * x + y * y + z * z).sqrt().max(1e-6);
            let (x, y, z) = (x / len, y / len, z / len);
            let t = 1.0 - c;
            let mut m = [0.0; 16];
            m[0] = c + x * x * t;
            m[1] = y * x * t + z * s;
            m[2] = z * x * t - y * s;
            m[4] = x * y * t - z * s;
            m[5] = c + y * y * t;
            m[6] = z * y * t + x * s;
            m[8] = x * z * t + y * s;
            m[9] = y * z * t - x * s;
            m[10] = c + z * z * t;
            m[15] = 1.0;
            m
        }
        TransformFn::Skew(ax, ay) => {
            let mut m = mat4_identity();
            // `skew(ax, ay)` is `matrix(1, tan(ay), tan(ax), 1, 0, 0)`.
            m[4] = ax.degrees().to_radians().tan();
            m[1] = ay.as_ref().map_or(0.0, |a| a.degrees().to_radians().tan());
            m
        }
        TransformFn::SkewX(a) => {
            let mut m = mat4_identity();
            m[4] = a.degrees().to_radians().tan();
            m
        }
        TransformFn::SkewY(a) => {
            let mut m = mat4_identity();
            m[1] = a.degrees().to_radians().tan();
            m
        }
        TransformFn::Matrix(a, b, c, d, tx, ty) => {
            let mut m = [0.0; 16];
            m[0] = *a;
            m[1] = *b;
            m[4] = *c;
            m[5] = *d;
            m[12] = *tx;
            m[13] = *ty;
            m[10] = 1.0;
            m[15] = 1.0;
            m
        }
        TransformFn::Matrix3d(m) => *m,
        TransformFn::Perspective(d) => {
            let d = px(d, base::Au::ZERO);
            if d > 0.0 {
                mat4_perspective(d)
            } else {
                mat4_identity()
            }
        }
    }
}

/// The translation part of `m` when it is a pure translation, else `None`.
fn mat4_pure_translation(m: &[f32; 16]) -> Option<[f32; 3]> {
    let is_translation = m[0] == 1.0
        && m[5] == 1.0
        && m[10] == 1.0
        && m[15] == 1.0
        && m[1] == 0.0
        && m[2] == 0.0
        && m[3] == 0.0
        && m[4] == 0.0
        && m[6] == 0.0
        && m[7] == 0.0
        && m[8] == 0.0
        && m[9] == 0.0
        && m[11] == 0.0
        && m[14] == 0.0;
    is_translation.then_some([m[12], m[13], m[14]])
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use crate::style::Angle;
    use crate::{Length, Transform, TransformFn};
    use base::{Au, Size};

    const BOX: Size = Size {
        width: Au(100 * 60),
        height: Au(100 * 60),
    };

    /// Asserts two column-major matrices are equal within a small epsilon.
    fn m_eq(a: &[f32; 16], b: &[f32; 16]) {
        for i in 0..16 {
            assert!(
                (a[i] - b[i]).abs() < 1e-4,
                "matrix index {i}: {} vs {}",
                a[i],
                b[i]
            );
        }
    }

    fn at_corner(transform: Vec<TransformFn>) -> Transform {
        Transform {
            transform: Rc::from(transform),
            origin: (Length::ZERO, Length::ZERO),
            ..Default::default()
        }
    }

    /// A transform with no functions is the identity and reports `None`.
    #[test]
    fn no_transform_is_none() {
        assert_eq!(Transform::default().matrix(BOX), None);
    }

    /// `scale(1)` is still a transform: it makes a reference frame.
    #[test]
    fn an_identity_function_is_still_a_transform() {
        let t = at_corner(vec![TransformFn::Scale(1.0, None)]);
        assert!(t.matrix(BOX).is_some());
    }

    /// `translate(tx, ty)` produces a pixel-snapped translation matrix.
    #[test]
    fn pure_translate_is_pixel_snapped() {
        let t = at_corner(vec![TransformFn::Translate(
            Length::px(10.3),
            Some(Length::px(20.7)),
        )]);
        m_eq(
            &t.matrix(BOX).unwrap(),
            &[
                1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 10.0, 21.0, 0.0, 1.0,
            ],
        );
    }

    /// Translation percentages refer to the border box.
    #[test]
    fn translate_percentages_use_the_box() {
        let t = at_corner(vec![TransformFn::Translate(
            Length::Percent(50.0),
            Some(Length::Percent(10.0)),
        )]);
        let m = t.matrix(BOX).unwrap();
        assert_eq!((m[12], m[13]), (50.0, 10.0));
    }

    /// `rotate(90deg)` turns x toward y.
    #[test]
    fn rotate_rotates_about_the_origin() {
        let m = at_corner(vec![TransformFn::Rotate(Angle::Deg(90.0))])
            .matrix(BOX)
            .unwrap();
        assert!((m[0] - 0.0).abs() < 1e-4);
        assert!((m[1] - 1.0).abs() < 1e-4);
        assert!((m[4] - -1.0).abs() < 1e-4);
        assert!((m[5] - 0.0).abs() < 1e-4);
    }

    /// The default origin is the center: `rotate(45deg)` keeps it still.
    #[test]
    fn the_default_origin_is_the_center() {
        let t = Transform {
            transform: Rc::from([TransformFn::Rotate(Angle::Deg(45.0))]),
            ..Default::default()
        };
        let m = t.matrix(BOX).unwrap();
        let center = (
            m[0] * 50.0 + m[4] * 50.0 + m[12],
            m[1] * 50.0 + m[5] * 50.0 + m[13],
        );
        assert!((center.0 - 50.0).abs() < 1e-3 && (center.1 - 50.0).abs() < 1e-3);
        assert!((m[12] - 50.0).abs() < 1e-3, "tx {}", m[12]);
        assert!((m[13] - -20.7107).abs() < 1e-3, "ty {}", m[13]);
    }

    /// translate, rotate, scale, then `transform`, all about the origin.
    #[test]
    fn separate_properties_compose_before_the_list() {
        let t = Transform {
            transform: Rc::from([TransformFn::TranslateX(Length::px(10.0))]),
            translate: Some((Length::px(50.0), Some(Length::ZERO))),
            rotate: Some(Angle::Deg(90.0)),
            scale: Some((2.0, None)),
            ..Default::default()
        };
        let m = t.matrix(BOX).unwrap();
        assert!((m[12] - 200.0).abs() < 1e-3, "tx {}", m[12]);
        assert!((m[13] - -30.0).abs() < 1e-3, "ty {}", m[13]);
    }

    /// `perspective` writes `-1/d` into the bottom row's z entry, about the
    /// perspective origin.
    #[test]
    fn perspective_is_the_m34_entry() {
        let t = Transform {
            perspective: Some(Length::px(100.0)),
            ..Default::default()
        };
        let m = t.perspective_matrix(BOX).unwrap();
        assert!((m[11] - -0.01).abs() < 1e-6);
        assert_eq!(m[14], 0.0);
    }

    /// `rotateY` follows `rotate3d(0, 1, 0, a)`.
    #[test]
    fn rotate_y_matches_rotate3d() {
        let a = Angle::Deg(30.0);
        let y = at_corner(vec![TransformFn::RotateY(a.clone())])
            .matrix(BOX)
            .unwrap();
        let r = at_corner(vec![TransformFn::Rotate3d(0.0, 1.0, 0.0, a)])
            .matrix(BOX)
            .unwrap();
        m_eq(&y, &r);
        let x = at_corner(vec![TransformFn::RotateX(Angle::Deg(30.0))])
            .matrix(BOX)
            .unwrap();
        let r = at_corner(vec![TransformFn::Rotate3d(1.0, 0.0, 0.0, Angle::Deg(30.0))])
            .matrix(BOX)
            .unwrap();
        m_eq(&x, &r);
    }

    /// `matrix3d(...)` passes its 16 values straight through.
    #[test]
    fn matrix3d_passes_through() {
        let values = [
            1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0, 11.0, 12.0, 13.0, 14.0, 15.0, 16.0,
        ];
        let m = at_corner(vec![TransformFn::Matrix3d(values)])
            .matrix(BOX)
            .unwrap();
        m_eq(&m, &values);
    }
}
