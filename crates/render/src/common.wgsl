// Shared by every pipeline, prepended to each shader. Instances arrive in
// root target pixels (y down). A pass that draws into an opacity layer
// sees only its part of the root target, starting at `origin`.

struct Target {
    // The pass's target size, in pixels.
    size: vec2<f32>,
    // Where the pass's target sits in the root target, in pixels.
    origin: vec2<f32>,
};

@group(0) @binding(0) var<uniform> pass_target: Target;

// The corner of `quad` (left, top, right, bottom) that vertex `vertex` of
// two triangles uses, as 0 or 1 per axis.
fn corner_of(vertex: u32) -> vec2<f32> {
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(0.0, 1.0),
        vec2<f32>(0.0, 1.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(1.0, 1.0),
    );
    return corners[vertex];
}

// A point in root target pixels, in the pass's clip space.
fn to_clip_space(p: vec2<f32>) -> vec4<f32> {
    let local = p - pass_target.origin;
    return vec4<f32>(
        local.x / pass_target.size.x * 2.0 - 1.0,
        1.0 - local.y / pass_target.size.y * 2.0,
        0.0,
        1.0,
    );
}

// A fragment's position in root target pixels: the pixel center.
fn root_position(fragment: vec4<f32>) -> vec2<f32> {
    return fragment.xy + pass_target.origin;
}

// Signed distance from `p` to the box (left, top, right, bottom) with its
// corners rounded by `radii` (top left, top right, bottom right, bottom
// left): negative inside.
fn rounded_box_distance(p: vec2<f32>, box: vec4<f32>, radii: vec4<f32>) -> f32 {
    let center = (box.xy + box.zw) * 0.5;
    let half_size = max((box.zw - box.xy) * 0.5, vec2<f32>(0.0));
    let left = p.x < center.x;
    let r = select(select(radii.z, radii.w, left), select(radii.y, radii.x, left), p.y < center.y);
    let q = abs(p - center) - half_size + vec2<f32>(r);
    return length(max(q, vec2<f32>(0.0))) + min(max(q.x, q.y), 0.0) - r;
}

// How much of a pixel lies inside a shape `distance` away: one pixel of
// falloff antialiases the edge.
fn coverage(distance: f32) -> f32 {
    return clamp(0.5 - distance, 0.0, 1.0);
}

// How much of the pixel at `p` the clip box keeps.
fn clip_coverage(p: vec2<f32>, clip: vec4<f32>, radius: f32) -> f32 {
    return coverage(rounded_box_distance(p, clip, vec4<f32>(radius)));
}
