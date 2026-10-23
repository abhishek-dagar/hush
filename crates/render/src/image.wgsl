// Images and opacity layers: instanced quads sampling a premultiplied RGBA
// texture, faded by an alpha and cut by the clip box as in `rect.wgsl`.
// The pipeline blends premultiplied.

@group(1) @binding(0) var image: texture_2d<f32>;
@group(1) @binding(1) var image_sampler: sampler;

struct ImageIn {
    @builtin(vertex_index) vertex: u32,
    @location(0) quad: vec4<f32>,
    // The part of the texture drawn, as UVs: left, top, right, bottom.
    @location(1) uv: vec4<f32>,
    @location(2) clip: vec4<f32>,
    // The alpha (a layer's opacity), then the clip's corner radius.
    @location(3) params: vec2<f32>,
};

struct ImageOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) clip: vec4<f32>,
    @location(2) params: vec2<f32>,
};

@vertex
fn vs_main(in: ImageIn) -> ImageOut {
    let corner = corner_of(in.vertex);
    var out: ImageOut;
    out.position = to_clip_space(mix(in.quad.xy, in.quad.zw, corner));
    out.uv = mix(in.uv.xy, in.uv.zw, corner);
    out.clip = in.clip;
    out.params = in.params;
    return out;
}

@fragment
fn fs_main(in: ImageOut) -> @location(0) vec4<f32> {
    let texel = textureSample(image, image_sampler, in.uv);
    let kept = clip_coverage(root_position(in.position), in.clip, in.params.y);
    return texel * (in.params.x * kept);
}
