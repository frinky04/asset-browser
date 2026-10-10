# Handoff: 3D model embedding search (assessment + prototype plan)

Date: 2026-08-22
Workspace: `C:\Users\cuddl\Documents\Dev\Research\embed textures`
Companion docs: `embed-textures-handoff.md` (API format, image pricing, app architecture recs), `embed-audio-handoff.md`. Not repeated here.

## Status
Assessment only — no 3D files present yet, nothing prototyped. User is deciding whether to drop a `test-models/` set.

## Environment
- Blender 5.1 installed at `C:\Program Files\Blender Foundation\` (not on PATH).
- Python 3.14 with PIL/numpy; no trimesh/pyrender installed.
- OpenRouter key in env `OPENROUTER_API_KEY` (never print). Model fixed by user: `google/gemini-embedding-2`.

## Key conclusion
No hosted embedding API (OpenRouter included) accepts 3D input. Native 3D encoders (Uni3D, OpenShape, ULIP-2) are local-only research models, point-cloud based, lose materials, and lag image models. For a BYOK browser app the practical route is **render → embed as images**, reusing the validated image pipeline (`embed.py`, message-style `image_url` input, flat 258 tokens ≈ $0.00012 per image).

## Proposed approach
1. Render 4–6 turntable views (+ top) per model, neutral studio lighting; textured if materials load, else grey clay.
2. Option (a) contact sheet: tile views into one 2×3 image → 1 request/model, same cost as a texture. Option (b) per-view vectors, mean/max pooled → ~6× cost, better rotation invariance.
3. Hybrid: include filename/folder + cheap mesh stats (tri count, bbox dims, has textures) as text in the same request.
4. Queries: text; image (screenshot/concept art vs contact sheet); model-as-query = render identically.

## Cost/time estimates
- API: ~$0.12 per 1k models (contact sheet) — negligible.
- Rendering dominates: Blender headless ~0.5–2 s/model (GPU); in-app offscreen three.js/Filament 10–50 ms/model. 100k models ≈ 1 h in-app vs ~1 day via Blender. Share this work with thumbnail generation.
- Formats: GLB/glTF easiest; FBX/BLEND/USD need Blender or Assimp. Expect failures (missing textures, bad scale, degenerate UVs) → clay fallback.

## Prototype plan (next session)
1. Wait for / request 50–100 models in `test-models/` (GLB preferred, some FBX/OBJ for format coverage).
2. Write a Blender `--background --python` script: normalize scale, 6-view render (clay + textured), save contact sheet PNG + individual views.
3. Embed: contact-sheet vs pooled-per-view, clay vs textured, with/without hybrid text.
4. Tests: text queries, image query, model-as-query, minimum view count (1/3/6), clay vs textured for text retrieval.
5. Report cost, render time per model, recommended view count/resolution.

## Later option
Local native 3D: Uni3D (ViT-g, ~1B params) is the strongest open model; treat as add-on since rendering is needed for thumbnails anyway.

## Suggested skills
- `code-review` / `simplify` after implementing in the app repo.
- `run` if testing the render pipeline inside the app.
- `claude-api` only if Anthropic models become involved.
