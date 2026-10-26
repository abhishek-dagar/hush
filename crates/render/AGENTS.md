# render

## Overview

The wgpu renderer. It owns the GPU device and the window's drawing surface, and will draw display lists sent from the content process. Today it only clears the window to a blank page; drawing display lists arrives with feature 5.

## Key files

| File | Owns |
|---|---|
| `src/lib.rs` | `Renderer` (device, queue, surface), `RenderError` |

## Conventions

- Linked into the browser process only (spec 0001, render boundary). The content process produces a `display-list` value and never draws.
- Backends are Metal, DX12 and Vulkan. The GL backend is compiled out on purpose to save bundle size; don't re-enable it through workspace features.
- Surface states the frame can't use right now (timeout, occluded, outdated) skip the frame and return `Ok`. Only a lost surface is an error.

## Gotchas

- A zero sized window (minimized) must not reconfigure the surface; `resize` ignores it.
- Perf frame marks read `last_present_at`, which moves only when a frame presents (spec 0007). A skipped frame presents nothing, so timing one would fake a cold start or scroll sample.
- `wgpu` is one of the largest parts of the binary. Check bundle size before adding wgpu features.

## Agent skills

- [webgpu-specs](../../../.agents/skills/webgpu-specs/): `gfx-rs/wgpu`, WebGPU and WGSL spec reference

## Related specs

- [0001 Stack and architecture](../../../docs/specs/0001-stack-architecture/index.md)

_Drafted by /audit from the repo, worth a quick human pass. Edit freely: once a line stops matching this draft, later runs treat it as curated and will flag rather than overwrite it._
