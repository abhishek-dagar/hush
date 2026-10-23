// Glyphs: instanced quads that sample the glyph atlas's coverage and fill
// it with the text color, cut by the clip box as in `rect.wgsl`.

@group(1) @binding(0) var atlas: texture_2d<f32>;
@group(1) @binding(1) var atlas_sampler: sampler;

struct GlyphIn {
    @builtin(vertex_index) vertex: u32,
    @location(0) quad: vec4<f32>,
    // The atlas texels, as UVs: left, top, right, bottom.
    @location(1) uv: vec4<f32>,
    @location(2) color: vec4<f32>,
    @location(3) clip: vec4<f32>,
    @location(4) radius: f32,
};

struct GlyphOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) clip: vec4<f32>,
    @location(3) radius: f32,
};

@vertex
fn vs_main(in: GlyphIn) -> GlyphOut {
    let corner = corner_of(in.vertex);
    var out: GlyphOut;
    out.position = to_clip_space(mix(in.quad.xy, in.quad.zw, corner));
    out.uv = mix(in.uv.xy, in.uv.zw, corner);
    out.color = in.color;
    out.clip = in.clip;
    out.radius = in.radius;
    return out;
}

@fragment
fn fs_main(in: GlyphOut) -> @location(0) vec4<f32> {
    let ink = textureSample(atlas, atlas_sampler, in.uv).r;
    let kept = clip_coverage(root_position(in.position), in.clip, in.radius);
    return vec4<f32>(in.color.rgb, in.color.a * ink * kept);
}
