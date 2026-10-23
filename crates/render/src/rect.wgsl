// Rects and borders (spec 0004, child 0005, Renderer pipelines): one
// instanced quad each, colored, cut to a rounded box. A border side, dash
// or dot is a quad too; the fragment keeps only the border's ring, and
// only the part of it the quad's side owns.

// What an instance draws.
const FILL: u32 = 0u;
// The whole ring between the box and the box inset by `widths`.
const RING: u32 = 1u;
// One side of the ring: 2 top, 3 right, 4 bottom, 5 left.
const SIDE_TOP: u32 = 2u;
// A dot: the circle that fills the quad.
const DOT: u32 = 6u;

struct ShapeIn {
    @builtin(vertex_index) vertex: u32,
    // Left, top, right, bottom: what the quad covers.
    @location(0) quad: vec4<f32>,
    @location(1) color: vec4<f32>,
    // The box the fill or the ring's outer edge follows.
    @location(2) box: vec4<f32>,
    // Its corner radii: top left, top right, bottom right, bottom left.
    @location(3) radii: vec4<f32>,
    // The border widths: top, right, bottom, left.
    @location(4) widths: vec4<f32>,
    @location(5) clip: vec4<f32>,
    // The clip's corner radius, then the kind.
    @location(6) params: vec2<f32>,
};

struct ShapeOut {
    @builtin(position) position: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) box: vec4<f32>,
    @location(2) radii: vec4<f32>,
    @location(3) widths: vec4<f32>,
    @location(4) clip: vec4<f32>,
    @location(5) quad: vec4<f32>,
    @location(6) @interpolate(flat) params: vec2<f32>,
};

@vertex
fn vs_main(in: ShapeIn) -> ShapeOut {
    let corner = corner_of(in.vertex);
    var out: ShapeOut;
    out.position = to_clip_space(mix(in.quad.xy, in.quad.zw, corner));
    out.color = in.color;
    out.box = in.box;
    out.radii = in.radii;
    out.widths = in.widths;
    out.clip = in.clip;
    out.quad = in.quad;
    out.params = in.params;
    return out;
}

// Which side owns `p` in a border: the one whose outer edge is nearest
// relative to its width, which splits each corner on the line from its
// outer to its inner corner. A side with no width owns nothing.
fn owner(p: vec2<f32>, box: vec4<f32>, widths: vec4<f32>) -> u32 {
    let reach = vec4<f32>(p.y - box.y, box.z - p.x, box.w - p.y, p.x - box.x);
    let far = vec4<f32>(1.0e30);
    let d = select(reach / max(widths, vec4<f32>(1.0e-4)), far, widths <= vec4<f32>(0.0));
    var side = 0u;
    var best = d.x;
    if d.y < best { side = 1u; best = d.y; }
    if d.z < best { side = 2u; best = d.z; }
    if d.w < best { side = 3u; }
    return side;
}

@fragment
fn fs_main(in: ShapeOut) -> @location(0) vec4<f32> {
    let p = root_position(in.position);
    let kind = u32(in.params.y + 0.5);
    var alpha = 1.0;
    if kind == DOT {
        let center = (in.quad.xy + in.quad.zw) * 0.5;
        alpha = coverage(length(p - center) - (in.quad.z - in.quad.x) * 0.5);
    } else {
        alpha = coverage(rounded_box_distance(p, in.box, in.radii));
        if kind != FILL {
            let w = in.widths;
            let inner = vec4<f32>(in.box.x + w.w, in.box.y + w.x, in.box.z - w.y, in.box.w - w.z);
            let inner_radii = max(
                in.radii - vec4<f32>(max(w.w, w.x), max(w.x, w.y), max(w.y, w.z), max(w.z, w.w)),
                vec4<f32>(0.0),
            );
            let empty = inner.z <= inner.x || inner.w <= inner.y;
            let inside = coverage(rounded_box_distance(p, inner, inner_radii));
            alpha *= select(1.0 - inside, 1.0, empty);
            if kind >= SIDE_TOP && owner(p, in.box, w) != kind - SIDE_TOP {
                alpha = 0.0;
            }
        }
    }
    alpha *= clip_coverage(p, in.clip, in.params.x);
    return vec4<f32>(in.color.rgb, in.color.a * alpha);
}
