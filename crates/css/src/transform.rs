//! 4 by 4 column-major matrix composition for transforms (spec 0010, AC-19).
//! Index is `row + 4 * column`, the bottom row is `[0, 0, 0, 1]`.

use crate::{Length, Transform, TransformFn};

/// The 4 by 4 column-major matrix for this transform, or `None` when it is
/// the identity (no transform at all).
///
/// Composed about `transform-origin`: the separate `translate`, `rotate`
/// and `scale` properties apply in that order before `transform`, then each
/// `transform` function in declaration order (spec 0010, AC-19). The result
/// maps box-local coords to border-box-local coords (transformed about the
/// origin); callers place it in parent space by translating by the box's
impl Transform {
    /// The 4 by 4 column-major matrix for this transform, or `None` when it
    /// is the identity (no transform at all).
    ///
    /// Composed about `transform-origin`: the separate `translate`, `rotate`
    /// and `scale` properties apply in that order before `transform`, then
    /// each `transform` function in declaration order (spec 0010, AC-19).
    /// The result maps box-local coords to border-box-local coords
    /// (transformed about the origin); callers place it in parent space by
    /// translating by the box's border box origin (spec 0010, AC-20).
    pub fn matrix(&self) -> Option<[f32; 16]> {
        if !self.has_transform() {
            return None;
        }
        let mut m = mat4_identity();
        if let Some((x, y)) = &self.translate {
            m = mat4_mul(&m, &mat4_translate(length_px_x(x), length_px_y(y), 0.0));
        }
        if let Some(angle) = &self.rotate {
            m = mat4_mul(&m, &mat4_rotate_z(angle.degrees().to_radians()));
        }
        if let Some((x, y)) = &self.scale {
            // `scale(s)` scales both axes by `s`; `scale(sx, sy)` scales each.
            m = mat4_mul(&m, &mat4_scale(*x, y.unwrap_or(*x), 1.0));
        }
        for f in self.transform.iter() {
            m = mat4_mul(&m, &transform_fn_matrix(f));
        }
        let origin = (self.origin.x.to_f32_px(), self.origin.y.to_f32_px());
        // `mat4_mul(a, b)` is `b * a`, so compose right to left: move to the
        // origin, apply the transform, move back (spec 0010, AC-19).
        m = mat4_mul(&mat4_translate(-origin.0, -origin.1, 0.0), &m);
        m = mat4_mul(&m, &mat4_translate(origin.0, origin.1, 0.0));
        if mat4_is_identity(&m) {
            return None;
        }
        if let Some(t) = mat4_pure_translation(&m) {
            return Some(mat4_translate(t[0].round(), t[1].round(), t[2].round()));
        }
        Some(m)
    }
}

fn length_px_x(length: &Length) -> f32 {
    match length {
        Length::Au(au) => au.to_f32_px(),
        Length::Percent(_) | Length::Calc(_) => 0.0,
    }
}

fn length_px_y(y: &Option<Length>) -> f32 {
    y.as_ref().map_or(0.0, length_px_x)
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

fn mat4_is_identity(m: &[f32; 16]) -> bool {
    (m[0] - 1.0).abs() < f32::EPSILON
        && (m[5] - 1.0).abs() < f32::EPSILON
        && (m[10] - 1.0).abs() < f32::EPSILON
        && (m[15] - 1.0).abs() < f32::EPSILON
        && m[1] == 0.0
        && m[2] == 0.0
        && m[3] == 0.0
        && m[4] == 0.0
        && m[6] == 0.0
        && m[7] == 0.0
        && m[8] == 0.0
        && m[9] == 0.0
        && m[11] == 0.0
        && m[12] == 0.0
        && m[13] == 0.0
        && m[14] == 0.0
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

fn mat4_rotate_x(angle: f32) -> [f32; 16] {
    let (s, c) = angle.sin_cos();
    let mut m = mat4_identity();
    m[5] = c;
    m[6] = -s;
    m[9] = s;
    m[10] = c;
    m
}

fn mat4_rotate_y(angle: f32) -> [f32; 16] {
    let (s, c) = angle.sin_cos();
    let mut m = mat4_identity();
    m[0] = c;
    m[2] = s;
    m[8] = -s;
    m[10] = c;
    m
}

fn mat4_perspective(d: f32) -> [f32; 16] {
    let mut m = mat4_identity();
    m[14] = -1.0 / d;
    m
}

/// The matrix for one transform function (spec 0010, AC-19).
fn transform_fn_matrix(f: &TransformFn) -> [f32; 16] {
    match f {
        TransformFn::Translate(x, y) => {
            let y = length_px_y(y);
            mat4_translate(length_px_x(x), y, 0.0)
        }
        TransformFn::TranslateX(x) => mat4_translate(length_px_x(x), 0.0, 0.0),
        TransformFn::TranslateY(y) => mat4_translate(0.0, length_px_x(y), 0.0),
        TransformFn::Translate3d(x, y, z) => {
            mat4_translate(length_px_x(x), length_px_x(y), length_px_x(z))
        }
        TransformFn::TranslateZ(z) => mat4_translate(0.0, 0.0, length_px_x(z)),
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
            m[1] = ax.degrees().to_radians().tan();
            m[4] = ay.as_ref().map_or(0.0, |a| a.degrees().to_radians().tan());
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
        TransformFn::Perspective(d) => mat4_perspective(length_px_x(d)),
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
    use base::{Au, Point};

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

    /// A transform with no functions is the identity and reports `None`.
    #[test]
    fn no_transform_is_none() {
        let t = Transform::default();
        assert_eq!(t.matrix(), None);
    }

    /// `translate(tx, ty)` produces a pixel-snapped translation matrix.
    #[test]
    fn pure_translate_is_pixel_snapped() {
        let t = Transform {
            transform: Rc::from([TransformFn::Translate(
                Length::px(10.3),
                Some(Length::px(20.7)),
            )]),
            ..Default::default()
        };
        let m = t.matrix().unwrap();
        m_eq(
            &m,
            &[
                1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 10.0, 21.0, 0.0, 1.0,
            ],
        );
    }

    /// `scale(s)` scales both axes about the origin.
    #[test]
    fn scale_scales_both_axes() {
        let t = Transform {
            transform: Rc::from([TransformFn::Scale(2.0, None)]),
            ..Default::default()
        };
        let m = t.matrix().unwrap();
        m_eq(
            &m,
            &[
                2.0, 0.0, 0.0, 0.0, 0.0, 2.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
            ],
        );
    }

    /// `rotate(90deg)` rotates about the origin (counter-clockwise).
    #[test]
    fn rotate_rotates_about_the_origin() {
        let t = Transform {
            transform: Rc::from([TransformFn::Rotate(Angle::Deg(90.0))]),
            ..Default::default()
        };
        let m = t.matrix().unwrap();
        assert!((m[0] - 0.0).abs() < 1e-4);
        assert!((m[1] - 1.0).abs() < 1e-4);
        assert!((m[4] - -1.0).abs() < 1e-4);
        assert!((m[5] - 0.0).abs() < 1e-4);
        assert!((m[10] - 1.0).abs() < 1e-4);
        assert!((m[15] - 1.0).abs() < 1e-4);
    }

    /// `scale(s)` about an origin other than (0, 0) translates the result.
    #[test]
    fn scale_about_origin_translates() {
        let t = Transform {
            transform: Rc::from([TransformFn::Scale(2.0, None)]),
            origin: Point::new(Au::from_px(50.0), Au::from_px(50.0)),
            ..Default::default()
        };
        let m = t.matrix().unwrap();
        assert!((m[0] - 2.0).abs() < 1e-4);
        assert!((m[5] - 2.0).abs() < 1e-4);
        assert!((m[12] - -50.0).abs() < 1e-4);
        assert!((m[13] - -50.0).abs() < 1e-4);
        assert!((m[15] - 1.0).abs() < 1e-4);
    }

    /// `matrix3d(...)` passes its 16 values straight through.
    #[test]
    fn matrix3d_passes_through() {
        let values = [
            1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0, 11.0, 12.0, 13.0, 14.0, 15.0, 16.0,
        ];
        let t = Transform {
            transform: Rc::from([TransformFn::Matrix3d(values)]),
            ..Default::default()
        };
        let m = t.matrix().unwrap();
        m_eq(&m, &values);
    }
}
