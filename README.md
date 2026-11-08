<img src="assets/icon.svg" width="64" height="64" alt="">

# Asset Browser

A fast browser for a folder of game assets on Windows, built with [Rune](https://github.com/anomalyco/rune).

- Thumbnails for textures (png, jpg, bmp, tga, webp), models (glb, gltf, obj, dae, fbx) and audio (wav, ogg, mp3, flac).
- Search names and folders, filter by pack, type and format. See [Searching](#searching).
- Orbit, pan and zoom models. Pan, zoom and tile textures. Scrub audio.
- Drag anything out to a folder or another program. Drags hand over a staged copy, so the library file is never touched.
- Pick a smaller size for a texture to preview it and drag it out at that size, or set a size for all grid drags.
- Press **Edit** to change what gets dragged out. See [Editing](#editing).

```sh
cargo run --release
```

The library defaults to the dogfood folder. Use **Change library folder** in the app, or set `ASSET_BROWSER_ROOT`.

Thumbnails, the index and staged exports live in `%LOCALAPPDATA%\AssetBrowserV2`.

## Searching

Words all have to match, in any order, and `wood door` finds `woodDoor` and `wood_door`. Related words count too, after the literal matches, so `gun` finds pistols. A typo that finds nothing is corrected.

| Write | To get |
| --- | --- |
| `"old door"` | Exactly those whole words, nothing related |
| `-rust` | Leave it out. Works on fields too: `-is:data` |
| `door\|gate` | Either. Works in fields too: `is:model\|audio` |
| `is:model` `is:audio` `is:texture` | One type |
| `is:normal` `is:roughness` `is:metallic` `is:ao` `is:height` `is:emissive` `is:specular` `is:opacity` `is:color` | A texture map, by its name. `is:data` is any but colour |
| `is:square` `is:pot` `is:wide` `is:tall` `is:broken` | Texture shape, or files that can't be previewed |
| `pack:mega` `folder:props` `name:door` | Only look there |
| `ext:fbx` or `.fbx` | One format, including copies normally hidden behind a better format |
| `size:>1mb` `size:1mb..10mb` | File size |
| `res:>=2k` `w:512` `h:<256` | Texture pixels. `res` is the longest side |
| `tris:<500` | Model triangles |
| `dur:<2s` | Audio length |
| `age:<30d` | Last changed (`h`, `d`, `w`, `mo`, `y`) |

Pixels, triangles and length come from thumbnailing, so those filters fill in as it works through the library.

The list under the search box completes these: arrows pick a row, Tab or Enter takes it.

## Editing

**Edit** in the inspector opens controls for the selected asset. Edits never touch the library file: the preview and the drag both use a rendered copy, and each asset remembers its edits until the app closes. A badge on the tile shows which assets have some, and grid drags use them too. **Original** flips the preview back to compare.

| Asset | Controls | Leaves as |
| --- | --- | --- |
| Audio | Drag the waveform's markers to trim. Under Loop, slide the join in the seam view, crossfade it, or let **Find best join** line it up. Under Compress, set quality, sample rate and channels | Ogg Vorbis |
| Textures | Palette size, dithering and encoder effort, on top of the size ladder | PNG |
| Models | Every material's roughness, metallic, emissive, normal strength, tint and texture size, or a quick look: Matte, Satin, Gloss, Metal. Texture filtering for the whole model, or per texture by clicking its thumbnail | GLB |

Editing a GLB or glTF keeps its nodes, skins and animation. Other model formats leave as one static mesh. Each material shows on a little rendered ball with its maps beside it; hover one to pick it out on the model.

## Keys

Arrows move the selection. Space plays or pauses audio. `/` focuses search and Escape clears it.

## Checking it headlessly

```sh
cargo test
target/release/asset-browser --snap out.png --scene model --size 1520x900
```

Scenes: `textures`, `models`, `audio`, `search`, `small`, `texture`, `tile`, `tile-seams`, `big-texture`, `model`, `sound`, `edit-trim`, `edit-loop`, `edit-compress`, `edit-texture`, `edit-model`. Set `AB_FRAME_MS=16` with a `--script` to scroll against the real thumbnail workers at 60 fps.
