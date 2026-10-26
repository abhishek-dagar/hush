//! A stable text dump of a display list, for snapshot tests (spec 0002,
//! AC-17). The same list always prints the same bytes. Scroll ids pack raw
//! node ids, so they are left out; spatial nodes and clips are named by
//! their index in the list.

use std::fmt::Write;

use crate::{
    Binding, BorderStyle, ColorF, DisplayList, ItemKind, PointF, RectF, SizeF, SpatialKind,
};

impl DisplayList {
    /// The list as indented text: the spatial tree, the clips, then the items.
    pub fn dump(&self) -> String {
        let mut out = String::new();
        // Writing to a `String` cannot fail.
        let _ = self.write_dump(&mut out);
        out
    }

    fn write_dump(&self, out: &mut String) -> std::fmt::Result {
        writeln!(
            out,
            "display list page={} epoch={} viewport={}",
            self.page.0,
            self.epoch.0,
            size(self.viewport)
        )?;
        writeln!(out, "spatial")?;
        for (index, node) in self.spatial.iter().enumerate() {
            let parent = node.parent.map_or("-".into(), |p| p.0.to_string());
            write!(out, "  s{index} parent={parent} ")?;
            match &node.kind {
                SpatialKind::Root => writeln!(out, "root")?,
                SpatialKind::ScrollFrame {
                    frame_rect,
                    content_size,
                    offset,
                    ..
                } => writeln!(
                    out,
                    "scroll-frame port={} content={} offset={} generation={}",
                    rect(*frame_rect),
                    size(*content_size),
                    point(PointF {
                        x: offset.x,
                        y: offset.y
                    }),
                    offset.generation
                )?,
                SpatialKind::ReferenceFrame { transform } => {
                    let matrix = match transform {
                        Binding::Static(m) => *m,
                        Binding::Bound { value, .. } => *value,
                    };
                    let values: Vec<String> = matrix.iter().map(|v| num(*v)).collect();
                    writeln!(out, "reference-frame transform=[{}]", values.join(" "))?;
                }
                SpatialKind::Sticky {
                    sticky_rect,
                    margins,
                    ..
                } => writeln!(
                    out,
                    "sticky rect={} margins={:?} offset=...",
                    rect(*sticky_rect),
                    margins
                )?,
            }
        }
        writeln!(out, "clips")?;
        for (index, clip) in self.clips.iter().enumerate() {
            let parent = clip.parent.map_or("-".into(), |p| format!("c{}", p.0));
            write!(
                out,
                "  c{index} parent={parent} spatial=s{} rect={}",
                clip.spatial.0,
                rect(clip.rect)
            )?;
            if let Some(radii) = clip.radii {
                write!(out, " radii={}", radii.map(num).join(" "))?;
            }
            writeln!(out)?;
        }
        writeln!(out, "items")?;
        let mut depth = 1;
        for item in &self.items {
            if item.kind == ItemKind::PopStackingContext {
                depth = (depth - 1).max(1);
            }
            let clip = item.clip.map_or("-".into(), |c| format!("c{}", c.0));
            write!(
                out,
                "{:indent$}s{} clip={clip} ",
                "",
                item.spatial.0,
                indent = depth * 2
            )?;
            match &item.kind {
                ItemKind::Rect {
                    rect: r,
                    color: c,
                    radii,
                } => {
                    write!(out, "rect {} {}", rect(*r), color(*c))?;
                    if let Some(radii) = radii {
                        write!(out, " radii={}", radii.map(num).join(" "))?;
                    }
                    writeln!(out)?;
                }
                ItemKind::Border {
                    rect: r,
                    widths,
                    colors,
                    styles,
                    radii,
                } => {
                    write!(
                        out,
                        "border {} widths={} colors={} styles={}",
                        rect(*r),
                        widths.map(num).join(" "),
                        colors.map(color).join(" "),
                        styles.map(style).join(" ")
                    )?;
                    if let Some(radii) = radii {
                        write!(out, " radii={}", radii.map(num).join(" "))?;
                    }
                    writeln!(out)?;
                }
                ItemKind::Text {
                    font,
                    size: font_size,
                    color: c,
                    glyphs,
                } => {
                    writeln!(
                        out,
                        "text font={} size={} {} glyphs={}",
                        font.0,
                        num(*font_size),
                        color(*c),
                        glyphs.len()
                    )?;
                }
                ItemKind::Image { key, rect: r } => {
                    writeln!(out, "image key={} {}", key.0, rect(*r))?;
                }
                ItemKind::PushStackingContext {
                    opacity,
                    blend_mode,
                } => {
                    writeln!(
                        out,
                        "push-stacking-context opacity={} blend={blend_mode:?}",
                        num(*opacity)
                    )?;
                    depth += 1;
                }
                ItemKind::PopStackingContext => writeln!(out, "pop-stacking-context")?,
            }
        }
        Ok(())
    }
}

/// A number with at most two decimals and no trailing zeros, so float noise
/// below a hundredth of a pixel never changes a dump.
fn num(value: f32) -> String {
    let text = format!("{value:.2}");
    let text = text.trim_end_matches('0').trim_end_matches('.');
    match text {
        "-0" => "0".into(),
        other => other.into(),
    }
}

fn point(p: PointF) -> String {
    format!("{},{}", num(p.x), num(p.y))
}

fn size(s: SizeF) -> String {
    format!("{}x{}", num(s.width), num(s.height))
}

fn rect(r: RectF) -> String {
    format!("({} {})", point(r.origin), size(r.size))
}

fn color(c: ColorF) -> String {
    let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!(
        "#{:02x}{:02x}{:02x}{:02x}",
        byte(c.r),
        byte(c.g),
        byte(c.b),
        byte(c.a)
    )
}

fn style(s: BorderStyle) -> String {
    format!("{s:?}").to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_print_without_float_noise() {
        assert_eq!(num(8.0), "8");
        assert_eq!(num(0.1 + 0.2), "0.3");
        assert_eq!(num(-0.001), "0");
        assert_eq!(num(12.345), "12.35");
    }
}
