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
rather than guessing at descriptions. `semantic.currentByType` is the per-type coverage: a type
with few indexed assets next to its `typeCounts` total needs name search.

**2. Search.** Describe what the game needs, not a file name:

```
ab search --semantic "mossy cobblestone wall, seamless" --type tex --limit 8 --sheet sheet.png
ab search --semantic "heavy wooden door creak" --type audio --max-sec 4 --limit 6 --sheet sheet.png
ab search "barrel" --type model --limit 12 --sheet sheet.png        # name search
ab search --similar <id> --limit 8                                   # more like a chosen one
```

`--semantic` needs an index and an OpenRouter key (exit 3 explains which is missing); `--similar`
needs only the index. `--min-sec/--max-sec` filter sounds by length. Name search is a loose
subsequence match — a short query returns near-misses, so treat its `total` as an upper bound
and confirm visually; once the JSON shows the right folder, narrow with `path:<dir/from/relPath>` in the
query instead of paging with `--offset`. Texture results
include `width`/`height`; filter on those in your head before rendering previews.

**3. Look.** Read `sheet.png`. Tiles are numbered 1-based, row-major; tile *k* is `results[k-1]`
in the JSON. The bottom-left caption is `WxH`, seconds, or triangle count. A red tile could
not be rendered.

An all-audio page renders as stacked rows instead: header stats (length, peak, LUFS,
brightness, tonal/mixed/noisy, number of sounds), a dB waveform, and a log-frequency
spectrogram (100 / 1k / 10k Hz gridlines) on one fixed colour scale, so brighter means louder
across rows. Read the spectrogram to hear by eye:

- vertical line, full height: a transient (click, impact, footstep)
- horizontal stripes: tonal (creak, hum, whistle, voice); stripe spacing is the pitch. An
  impact can read "tonal" when its body rings (a wooden thump, a metal clank)
- bright smear across the height: noise (whoosh, wind, rain, hiss); a smear that sweeps up
  or down is a riser or a fall
- energy only near the bottom: boomy or muffled; reaching 10k: crisp or harsh
- red header text: the clip clips

Shaded, numbered bands are the distinct sounds in the clip; their numbers are what
`--segment` takes. Keep audio sheets to about 6 rows (`--limit 6`) so they stay legible, and
add `--time shared` when the comparison is length. `ab compare <id> <id> ... --sheet c.png`
draws a sheet for a shortlist gathered across several searches.

**4. Inspect.** Use ids from the search JSON (hand-typed relative paths must match exactly).
`ab inspect <id> --json` returns `width/height/channels/powerOfTwo`,
`triangles/vertices/bounds/formats`, or for audio `seconds`, `loudness` (LUFS), `peakDbfs`,
`spectrum`, `silence` and `segments` (start/end seconds of each distinct sound).
Add `--preview look.png` for a closer view (models: 6-view sheet; audio: one large strip).
For loops add `--loop`: `loopSeam` numbers and a view of the end running into the start. Both also take a path to any file on disk, which is how you
check your own export.

**5. Export.** Output paths come back absolute; every export appends to `<dir>/assets-used.json`
(source, pack, transform) — mention it to the user so attribution survives.

```
ab export <id> --to res/textures --resize 512           # PNG, downscale only — pick a source ≥ 512
ab export <id> --to assets/models --format obj          # sibling format; .mtl comes along
ab export <id> --to assets/sfx --trim-silence --loop    # seamless 16-bit WAV
ab export <id> --to assets/sfx --range 0.2-1.4          # seconds
ab export <id> --to assets/sfx --segment 3              # distinct sound 3 from inspect/sheet
ab export <id> --to assets/sfx --segment all --normalize -16   # one file per sound, even loudness
ab export <id> <id> <id> --to assets/sfx --normalize -16       # a set, all at one loudness
```

Existing files are never overwritten without `--yes`.

## Choosing well

- Match the engine: pixel-art or web games want `--resize` to 256–512 and `powerOfTwo`;
  3D props for low-spec targets want `triangles` in the low thousands.
- Prefer `--format glb` for 3D unless the engine needs FBX/OBJ.
- Short UI/footstep sounds search poorly by description; name search or `--similar` from one
  good hit works better. Takes of many hits (footsteps, impacts) are a source of single sounds:
  pick the cleanest numbered band and export it with `--segment`.
- A set of sounds wants matching loudness: export the whole set in one `ab export` call with
  `--normalize -16` (effects) or `-23` (quiet UI). Everything in that call lands at one level;
  when short transients cannot reach the target without peaking past -1 dBFS, the set is
  matched lower and `transform.note` says where. Separate calls are normalized separately.
- Loops: after `--loop`, run `ab inspect <output.wav> --preview seam.png --loop`. A clean seam
  shows no vertical flash at the marker and `loopSeam.levelJumpDb` under about 1.
- Ids are stable (hash of the library-relative path); keep them in notes so the user can re-run
  an export after changing their mind about size.

## Exit codes

0 ok · 1 error · 2 usage · 3 semantic unavailable · 4 asset not found. Messages on stderr say
what to try next.
