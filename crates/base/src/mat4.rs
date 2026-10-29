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
