<img src="assets/icon.svg" width="64" height="64" alt="">

# Asset Browser

A fast browser for a folder of game assets on Windows, built with [Rune](https://github.com/anomalyco/rune).

- Thumbnails for textures (png, jpg, bmp, tga, webp), models (glb, gltf, obj, dae, fbx) and audio (wav, ogg, mp3, flac).
- Search names and folders, filter by pack, type and format. See [Searching](#searching).
- Orbit, pan and zoom models. Pan, zoom and tile textures. Scrub audio.
- Drag anything out to a folder or another program. Drags hand over a staged copy, so the library file is never touched.
- Drag a texture at a smaller size from the size ladder, or set a size for all grid drags.

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

### Search by description

**Settings > Smart search** can also find textures and models by what they look like, so `something to sit on` finds chairs that aren't named that. It's off until you press **Download** (about 625 MB, once, from Hugging Face and npm). It then runs [EmbeddingGemma 2](https://huggingface.co/google/embeddinggemma-2) on this PC, and nothing about your library leaves it.

These results come after files whose names matched, and `pack:`, `is:` and `-word` still apply to them. Quoted searches stay literal. Sounds are found by name only.

Start a search with `~` to ignore names and rank only by how things look, as in `~old hospital tiles pack:mega`. **Find similar** in the inspector shows what looks most like the selected texture or model, written into the search box as `similar:"path"`.

Each file is looked at once. **Gentle** does that with up to four CPU threads. **Faster** uses the graphics card in short bursts, resting twice as long as it works. **Maximum** runs the graphics card with no rests, or every CPU thread if the card can't run it. All of them stop on battery, and the status bar says when it's happening. **Remove** deletes the model and the index.

The list under the search box completes these: arrows pick a row, Tab or Enter takes it.

## Keys

Arrows move the selection. Space plays or pauses audio. `/` focuses search and Escape clears it.

## Checking it headlessly

```sh
cargo test
target/release/asset-browser --snap out.png --scene model --size 1520x900
```

Scenes: `textures`, `models`, `audio`, `search`, `small`, `texture`, `tile`, `tile-seams`, `big-texture`, `model`, `sound`. Set `AB_FRAME_MS=16` with a `--script` to scroll against the real thumbnail workers at 60 fps.
