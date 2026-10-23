//! `Au`, the fixed point length every layout value uses (spec 0002, Units).

use std::iter::Sum;
use std::ops::{Add, AddAssign, Mul, Neg, Sub, SubAssign};

/// How many `Au` make one CSS pixel.
pub const AU_PER_PX: i32 = 60;

/// A length in 1/60 of a CSS pixel.
///
/// All arithmetic saturates at [`Au::MIN`] and [`Au::MAX`] (about ±35 million
/// px), so a huge length from a hostile page can never panic or wrap (AC-10).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Au(pub i32);

impl Au {
    /// Zero length.
    pub const ZERO: Self = Self(0);
    /// The largest length.
    pub const MAX: Self = Self(i32::MAX);
    /// The smallest (most negative) length.
    pub const MIN: Self = Self(i32::MIN);

    /// Converts CSS pixels, rounding to the nearest `Au`. Out of range values
    /// saturate and NaN becomes zero.
    pub fn from_px(px: f32) -> Self {
        // `as` from float to int saturates and maps NaN to 0, which is what we want.
        Self((px * AU_PER_PX as f32).round() as i32)
    }

    /// Converts to CSS pixels.
    pub fn to_f32_px(self) -> f32 {
        self.0 as f32 / AU_PER_PX as f32
    }

    /// Scales by a float factor, such as a percentage, saturating.
    pub fn scale_by(self, factor: f32) -> Self {
        Self((self.0 as f32 * factor).round() as i32)
    }
}

impl Add for Au {
    type Output = Self;
    fn add(self, rhs: Self) -> Self {
        Self(self.0.saturating_add(rhs.0))
    }
}

impl AddAssign for Au {
    fn add_assign(&mut self, rhs: Self) {
        *self = *self + rhs;
    }
}

impl Sub for Au {
    type Output = Self;
    fn sub(self, rhs: Self) -> Self {
        Self(self.0.saturating_sub(rhs.0))
    }
}

impl SubAssign for Au {
    fn sub_assign(&mut self, rhs: Self) {
        *self = *self - rhs;
    }
}

impl Mul<i32> for Au {
    type Output = Self;
    fn mul(self, rhs: i32) -> Self {
        Self(self.0.saturating_mul(rhs))
    }
}

impl Neg for Au {
    type Output = Self;
    fn neg(self) -> Self {
        Self(self.0.saturating_neg())
    }
}

impl Sum for Au {
    fn sum<I: Iterator<Item = Self>>(iter: I) -> Self {
        iter.fold(Self::ZERO, Add::add)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn px_round_trip() {
        assert_eq!(Au::from_px(1.0), Au(60));
        assert_eq!(Au::from_px(0.5).to_f32_px(), 0.5);
        assert_eq!(Au(90).to_f32_px(), 1.5);
    }

    #[test]
    fn arithmetic_saturates_instead_of_wrapping() {
        assert_eq!(Au::MAX + Au(1), Au::MAX);
        assert_eq!(Au::MIN - Au(1), Au::MIN);
        assert_eq!(Au::MAX * 1_000, Au::MAX);
        assert_eq!(Au::MIN * 2, Au::MIN);
        assert_eq!(Au::MAX * -2, Au::MIN);
        assert_eq!(-Au::MIN, Au::MAX);
        let mut a = Au::MAX;
        a += Au::MAX;
        assert_eq!(a, Au::MAX);
        assert_eq!([Au::MAX, Au::MAX].into_iter().sum::<Au>(), Au::MAX);
    }

    #[test]
    fn float_conversions_saturate() {
        assert_eq!(Au::from_px(f32::INFINITY), Au::MAX);
        assert_eq!(Au::from_px(-1e30), Au::MIN);
        assert_eq!(Au::from_px(f32::NAN), Au::ZERO);
        assert_eq!(Au::MAX.scale_by(10.0), Au::MAX);
    }
}
