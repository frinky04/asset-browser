# FBX/OBJ thumbnail and preview support

**Date:** 2026-07-22
**Status:** Approved

## Goal

Render real thumbnails and interactive previews for `.fbx` and `.obj` models,
which today short-circuit to a generic icon. `.dae` remains icon-only.

## Approach

Add [ufbx](https://github.com/ufbx/ufbx) v0.23.0 via FetchContent (single-file
C library, MIT — same pattern as cgltf). ufbx parses all FBX versions (binary +
ASCII) and OBJ through one API, and normalizes coordinate axes and units for us.

One new loader beside the existing one, same contract (worker-thread safe,
pure CPU, vertices pre-transformed to world space):

```cpp
CpuModel LoadGlbCpu(const std::wstring& path);     // existing (cgltf)
CpuModel LoadFbxObjCpu(const std::wstring& path);  // new (ufbx), lives in src/model_fbx.cpp
```

Everything downstream — orbit camera, D3D11 bake, `.abt` disk cache — is
format-agnostic and unchanged.

## Loader details

- Load options: right-handed Y-up axes, meters, generate missing normals,
  ignore animation, OBJ `.mtl` search by filename.
- Geometry: triangulate via ufbx, emit triangle soup per material part,
  transform positions/normals by each instance node's `geometry_to_world`.
- UVs: flip V (ufbx uses bottom-left origin; renderer expects glTF top-left).
- Textures, resolved per material base-color map, in order:
  1. embedded content in the file,
  2. `absolute_filename` as-is,
  3. `relative_filename` relative to the model's directory,
  4. `filename`'s basename next to the model file.
  Miss → material base-color factor only (renderer already supports this).
- `alphaMask` is enabled when the resolved texture's source had an alpha
  channel (covers PSX-style decals; FBX has no explicit alpha mode).
- Zero-triangle meshes and invisible nodes are skipped; empty result →
  `valid = false`.

## Integration

- `CMakeLists.txt`: declare ufbx, compile `ufbx.c` + `src/model_fbx.cpp`.
- `thumbs.cpp` `Request()`: icon short-circuit narrows to "Model that is not
  glb/gltf/fbx/obj". `Job.isGlb` becomes `Job.ext`; the worker dispatches to
  `LoadGlbCpu` or `LoadFbxObjCpu` by extension.
- `ui_preview.cpp`: same gate widening and dispatch for the interactive
  preview; failure text no longer implies GLB-only.

## Error handling

Any parse/decode failure returns `CpuModel{valid=false}` → `ThumbState::Failed`
→ generic icon, same as GLB. ufbx never throws and is fuzz-hardened, so
malformed pack files degrade to an icon, not a crash.

## Testing

Manual (no test harness in repo): build, browse the asset root's FBX/OBJ trees,
confirm tiles render, cache persists across restart, preview orbits, and a
corrupt `.fbx` falls back to the icon.
