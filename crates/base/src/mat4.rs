//! Column-major 4 by 4 matrix helpers for transforms (spec 0010, AC-19).
//! Index is `row + 4 * column`, the bottom row is `[0, 0, 0, 1]`.

/// Multiplies two column-major 4 by 4 matrices: `a` then `b` (`b * a`).
pub fn mat4_mul(a: &[f32; 16], b: &[f32; 16]) -> [f32; 16] {
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

/// The identity matrix.
pub fn mat4_identity() -> [f32; 16] {
    let mut m = [0.0; 16];
    m[0] = 1.0;
    m[5] = 1.0;
    m[10] = 1.0;
    m[15] = 1.0;
    m
}

/// A translation matrix.
pub fn mat4_translate(x: f32, y: f32, z: f32) -> [f32; 16] {
    let mut m = mat4_identity();
    m[12] = x;
    m[13] = y;
    m[14] = z;
    m
}

/// Inverts a column-major 4 by 4 affine matrix, or `None` when singular
/// (for example `scale(0)`).
pub fn mat4_inverse(m: &[f32; 16]) -> Option<[f32; 16]> {
    // Affine matrices only: the bottom row is [0, 0, 0, 1]. Invert the
    // upper-left 3 by 3 linear part and the translation.
    let mut linear = [[0.0; 3]; 3];
    for row in 0..3 {
        for col in 0..3 {
            linear[row][col] = m[row + 4 * col];
        }
    }
    let det = linear[0][0] * (linear[1][1] * linear[2][2] - linear[1][2] * linear[2][1])
        - linear[0][1] * (linear[1][0] * linear[2][2] - linear[1][2] * linear[2][0])
        + linear[0][2] * (linear[1][0] * linear[2][1] - linear[1][1] * linear[2][0]);
    if det.abs() < 1e-6 {
        return None;
    }
    let inv_det = 1.0 / det;
    let mut inv_linear = [[0.0; 3]; 3];
    inv_linear[0][0] = (linear[1][1] * linear[2][2] - linear[1][2] * linear[2][1]) * inv_det;
    inv_linear[0][1] = (linear[0][2] * linear[2][1] - linear[0][1] * linear[2][2]) * inv_det;
    inv_linear[0][2] = (linear[0][1] * linear[1][2] - linear[0][2] * linear[1][1]) * inv_det;
    inv_linear[1][0] = (linear[1][2] * linear[2][0] - linear[1][0] * linear[2][2]) * inv_det;
    inv_linear[1][1] = (linear[0][0] * linear[2][2] - linear[0][2] * linear[2][0]) * inv_det;
    inv_linear[1][2] = (linear[0][2] * linear[1][0] - linear[0][0] * linear[1][2]) * inv_det;
    inv_linear[2][0] = (linear[1][0] * linear[2][1] - linear[1][1] * linear[2][0]) * inv_det;
    inv_linear[2][1] = (linear[0][1] * linear[2][0] - linear[0][0] * linear[2][1]) * inv_det;
    inv_linear[2][2] = (linear[0][0] * linear[1][1] - linear[0][1] * linear[1][0]) * inv_det;
    let mut out = [0.0; 16];
    for row in 0..3 {
        for col in 0..3 {
            out[row + 4 * col] = inv_linear[row][col];
        }
    }
    out[15] = 1.0;
    // translation' = -inv_linear * translation
    let tx = m[12];
    let ty = m[13];
    let tz = m[14];
    out[12] = -(inv_linear[0][0] * tx + inv_linear[0][1] * ty + inv_linear[0][2] * tz);
    out[13] = -(inv_linear[1][0] * tx + inv_linear[1][1] * ty + inv_linear[1][2] * tz);
    out[14] = -(inv_linear[2][0] * tx + inv_linear[2][1] * ty + inv_linear[2][2] * tz);
    Some(out)
}

/// The point `(u, v)` on the `z = 0` plane that `m` projects onto `(x, y)`,
/// perspective divide included, or `None` when no point does: the plane is
/// seen edge on or the matrix is singular (as `scale(0)`), or the point is
/// behind the viewer (`w <= 0`). Hit testing maps the pointer back through
/// a transformed box with this (spec 0010, AC-21).
pub fn mat4_unproject(m: &[f32; 16], x: f32, y: f32) -> Option<(f32, f32)> {
    // A point on the plane is (u, v, 0, 1), so only columns 0, 1 and 3 and
    // rows 0, 1 and 3 matter: a 3 by 3 homography.
    let h = [
        [m[0], m[4], m[12]],
        [m[1], m[5], m[13]],
        [m[3], m[7], m[15]],
    ];
    let cofactor =
        |r0: usize, r1: usize, c0: usize, c1: usize| h[r0][c0] * h[r1][c1] - h[r0][c1] * h[r1][c0];
    let det = h[0][0] * cofactor(1, 2, 1, 2) - h[0][1] * cofactor(1, 2, 0, 2)
        + h[0][2] * cofactor(1, 2, 0, 1);
    if det.abs() < 1e-9 {
        return None;
    }
    // The adjugate's rows times (x, y, 1); dividing by det cancels in u/w.
    let u = cofactor(1, 2, 1, 2) * x - cofactor(0, 2, 1, 2) * y + cofactor(0, 1, 1, 2);
    let v = -cofactor(1, 2, 0, 2) * x + cofactor(0, 2, 0, 2) * y - cofactor(0, 1, 0, 2);
    let w = cofactor(1, 2, 0, 1) * x - cofactor(0, 2, 0, 1) * y + cofactor(0, 1, 0, 1);
    if w.abs() < 1e-12 {
        return None;
    }
    let (u, v) = (u / w, v / w);
    let depth = m[3] * u + m[7] * v + m[15];
    (depth > 0.0 && u.is_finite() && v.is_finite()).then_some((u, v))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An affine matrix maps back exactly.
    #[test]
    fn unproject_inverts_an_affine_map() {
        let mut m = mat4_translate(10.0, 20.0, 0.0);
        m[0] = 2.0;
        m[5] = 3.0;
        let (u, v) = mat4_unproject(&m, 14.0, 26.0).expect("invertible");
        assert!((u - 2.0).abs() < 1e-5 && (v - 2.0).abs() < 1e-5, "{u}, {v}");
    }

    /// A projected point maps back to where it came from.
    #[test]
    fn unproject_inverts_a_perspective_map() {
        let mut m = mat4_identity();
        m[3] = 0.005;
        let (px, py) = (40.0f32, 30.0f32);
        let w = m[3] * px + 1.0;
        let (u, v) = mat4_unproject(&m, px / w, py / w).expect("invertible");
        assert!((u - px).abs() < 1e-3 && (v - py).abs() < 1e-3, "{u}, {v}");
    }

    /// `scale(0)` maps no point back.
    #[test]
    fn unproject_of_a_singular_matrix_is_none() {
        let mut m = mat4_identity();
        m[0] = 0.0;
        m[5] = 0.0;
        assert_eq!(mat4_unproject(&m, 1.0, 1.0), None);
    }
}
