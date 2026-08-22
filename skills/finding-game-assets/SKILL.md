---
name: finding-game-assets
description: Searches the user's local game-asset library (textures, 3D models, sound effects, music) with the `ab` CLI and exports chosen files into a project, resized or trimmed as needed. Use when building or modifying a game and an asset is needed — a texture, sprite, material, model, prop, sound effect, ambience — or when the user mentions their asset library, asset packs, or `ab`.
---

# Finding game assets

`ab` is a headless front end for the user's Asset Browser library. Binary: `@AB_EXE@`.
Output is JSON when piped; `ab --help` lists every flag.

## Workflow

```
Asset Progress:
- [ ] 1. ab status            — confirm the library and whether semantic search is available
- [ ] 2. ab search --sheet    — get candidates plus one numbered image
- [ ] 3. Read the sheet       — pick by looking, not by file name
- [ ] 4. ab inspect           — check size / triangles / duration fits the target engine
- [ ] 5. ab export            — copy or convert into the project; note the manifest
```

**1. Status.** `ab status --json`. If `semantic.indexed` is false, only name search works; say so
rather than guessing at descriptions.

**2. Search.** Describe what the game needs, not a file name:

```
ab search --semantic "mossy cobblestone wall, seamless" --type tex --limit 8 --sheet sheet.png
ab search --semantic "heavy wooden door creak" --type audio --limit 6 --sheet sheet.png
ab search "barrel" --type model --limit 12 --sheet sheet.png        # name search
ab search --similar <id> --limit 8                                   # more like a chosen one
```

`--semantic` needs an index and an OpenRouter key (exit 3 explains which is missing); `--similar`
needs only the index. Name search is a loose subsequence match — a short query returns
near-misses, so treat its `total` as an upper bound and confirm visually. Texture results
include `width`/`height`; filter on those in your head before rendering previews.

**3. Look.** Read `sheet.png`. Tiles are numbered 1-based, row-major; tile *k* is `results[k-1]`
in the JSON. The bottom-left caption is `WxH`, seconds, or triangle count. A red tile could
not be rendered.

**4. Inspect.** Use ids from the search JSON (hand-typed relative paths must match exactly).
`ab inspect <id> --json` returns `width/height/channels/powerOfTwo`,
`triangles/vertices/bounds/formats`, or `seconds/sampleRate/trimmed`. Add `--preview look.png`
for a closer view (models: 6-view sheet).

**5. Export.** Output paths come back absolute; every export appends to `<dir>/assets-used.json`
(source, pack, transform) — mention it to the user so attribution survives.

```
ab export <id> --to res/textures --resize 512           # PNG, downscale only — pick a source ≥ 512
ab export <id> --to assets/models --format obj          # sibling format; .mtl comes along
ab export <id> --to assets/sfx --trim-silence --loop    # seamless 16-bit WAV
ab export <id> --to assets/sfx --range 0.2-1.4          # seconds
```

Existing files are never overwritten without `--yes`.

## Choosing well

- Match the engine: pixel-art or web games want `--resize` to 256–512 and `powerOfTwo`;
  3D props for low-spec targets want `triangles` in the low thousands.
- Prefer `--format glb` for 3D unless the engine needs FBX/OBJ.
- Short UI/footstep sounds search poorly by description; name search or `--similar` from one
  good hit works better.
- Ids are stable (hash of the library-relative path); keep them in notes so the user can re-run
  an export after changing their mind about size.

## Exit codes

0 ok · 1 error · 2 usage · 3 semantic unavailable · 4 asset not found. Messages on stderr say
what to try next.
