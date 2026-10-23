//! sRGB colors as `tokens.css` writes them, WCAG contrast, and OKLCH for
//! accent snapping (spec 0003, Accent).

/// A color in sRGB, each channel from 0 to 1, with straight (not
/// premultiplied) alpha.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rgba {
    /// Red.
    pub r: f32,
    /// Green.
    pub g: f32,
    /// Blue.
    pub b: f32,
    /// Alpha (opacity).
    pub a: f32,
}

impl Rgba {
    /// An opaque color from 8 bit channels.
    pub fn rgb8(r: u8, g: u8, b: u8) -> Self {
        Self {
            r: f32::from(r) / 255.0,
            g: f32::from(g) / 255.0,
            b: f32::from(b) / 255.0,
            a: 1.0,
        }
    }

    /// Parses the color forms the chrome CSS profile allows: `#rgb`,
    /// `#rrggbb`, `#rrggbbaa`, `rgb(r, g, b)`, `rgba(r, g, b, a)` and
    /// `transparent`. Anything else is `None`.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        if text.eq_ignore_ascii_case("transparent") {
            return Some(Self {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 0.0,
            });
        }
        if let Some(hex) = text.strip_prefix('#') {
            return parse_hex(hex);
        }
        let lower = text.to_ascii_lowercase();
        let args = lower
            .strip_prefix("rgba(")
            .or_else(|| lower.strip_prefix("rgb("))?
            .strip_suffix(')')?;
        let parts: Vec<&str> = args.split(',').map(str::trim).collect();
        let channel = |s: &str| {
            s.parse::<f32>()
                .ok()
                .filter(|v| (0.0..=255.0).contains(v))
                .map(|v| v / 255.0)
        };
        let (r, g, b) = match parts.as_slice() {
            [r, g, b] | [r, g, b, _] => (channel(r)?, channel(g)?, channel(b)?),
            _ => return None,
        };
        let a = match parts.get(3) {
            Some(a) => a.parse::<f32>().ok().filter(|a| (0.0..=1.0).contains(a))?,
            None => 1.0,
        };
        Some(Self { r, g, b, a })
    }

    /// This color painted over `below` (source over), as an opaque result
    /// when `below` is opaque.
    pub fn over(self, below: Self) -> Self {
        let a = self.a + below.a * (1.0 - self.a);
        if a == 0.0 {
            return below;
        }
        let mix = |top: f32, bottom: f32| (top * self.a + bottom * below.a * (1.0 - self.a)) / a;
        Self {
            r: mix(self.r, below.r),
            g: mix(self.g, below.g),
            b: mix(self.b, below.b),
            a,
        }
    }

    /// WCAG 2 relative luminance of the color channels (alpha ignored).
    pub fn luminance(self) -> f32 {
        0.2126 * linear(self.r) + 0.7152 * linear(self.g) + 0.0722 * linear(self.b)
    }

    /// OKLCH lightness, chroma and hue angle in degrees (0 to 360).
    pub fn oklch(self) -> (f32, f32, f32) {
        let (r, g, b) = (linear(self.r), linear(self.g), linear(self.b));
        let l = (0.412_221_46 * r + 0.536_332_55 * g + 0.051_445_995 * b).cbrt();
        let m = (0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b).cbrt();
        let s = (0.088_302_46 * r + 0.281_718_85 * g + 0.629_978_7 * b).cbrt();
        let lightness = 0.210_454_26 * l + 0.793_617_8 * m - 0.004_072_047 * s;
        let a = 1.977_998_5 * l - 2.428_592_2 * m + 0.450_593_7 * s;
        let b = 0.025_904_037 * l + 0.782_771_77 * m - 0.808_675_77 * s;
        let hue = b.atan2(a).to_degrees().rem_euclid(360.0);
        (lightness, a.hypot(b), hue)
    }
}

/// The WCAG 2 contrast ratio between two opaque colors, from 1 to 21.
pub fn contrast_ratio(a: Rgba, b: Rgba) -> f32 {
    let (la, lb) = (a.luminance(), b.luminance());
    let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

fn linear(c: f32) -> f32 {
    if c <= 0.040_45 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn parse_hex(hex: &str) -> Option<Rgba> {
    if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let digit = |i: usize| u8::from_str_radix(&hex[i..=i], 16).ok();
    let pair = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
    let (r, g, b, a) = match hex.len() {
        3 => (digit(0)? * 17, digit(1)? * 17, digit(2)? * 17, 255),
        6 => (pair(0)?, pair(2)?, pair(4)?, 255),
        8 => (pair(0)?, pair(2)?, pair(4)?, pair(6)?),
        _ => return None,
    };
    Some(Rgba {
        a: f32::from(a) / 255.0,
        ..Rgba::rgb8(r, g, b)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_profile_color_forms() {
        assert_eq!(Rgba::parse("#fff"), Some(Rgba::rgb8(255, 255, 255)));
        assert_eq!(Rgba::parse("#0A64D6"), Some(Rgba::rgb8(10, 100, 214)));
        assert_eq!(Rgba::parse("#00000080").map(|c| c.a), Some(128.0 / 255.0));
        assert_eq!(
            Rgba::parse("rgba(0, 0, 0, 0.06)"),
            Some(Rgba {
                a: 0.06,
                ..Rgba::rgb8(0, 0, 0)
            })
        );
        assert_eq!(Rgba::parse("rgb(255,0,0)"), Some(Rgba::rgb8(255, 0, 0)));
        assert_eq!(Rgba::parse("transparent").map(|c| c.a), Some(0.0));
    }

    #[test]
    fn rejects_anything_else() {
        for bad in [
            "red",
            "#12",
            "#ggg",
            "rgb(1, 2)",
            "rgba(0,0,0,2)",
            "hsl(0, 0%, 0%)",
        ] {
            assert_eq!(Rgba::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn contrast_matches_wcag() {
        let black = Rgba::rgb8(0, 0, 0);
        let white = Rgba::rgb8(255, 255, 255);
        assert!((contrast_ratio(black, white) - 21.0).abs() < 0.01);
        assert!((contrast_ratio(white, white) - 1.0).abs() < 0.001);
        // #767676 on white is the well known 4.54:1 gray.
        assert!((contrast_ratio(Rgba::rgb8(0x76, 0x76, 0x76), white) - 4.54).abs() < 0.01);
    }

    #[test]
    fn translucent_fill_blends_over_an_opaque_surface() {
        let fill = Rgba {
            a: 0.5,
            ..Rgba::rgb8(0, 0, 0)
        };
        let blended = fill.over(Rgba::rgb8(255, 255, 255));
        assert!((blended.r - 0.5).abs() < 0.001);
        assert!((blended.a - 1.0).abs() < 0.001);
    }

    #[test]
    fn oklch_hue_of_known_colors() {
        let (_, chroma, hue) = Rgba::rgb8(0x0A, 0x64, 0xD6).oklch();
        assert!((hue - 258.3).abs() < 0.5, "{hue}");
        assert!(chroma > 0.1);
        let (_, gray_chroma, _) = Rgba::rgb8(0x80, 0x80, 0x80).oklch();
        assert!(gray_chroma < 0.001);
    }
}
