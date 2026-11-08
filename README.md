<img src="assets/icon.svg" width="64" height="64" alt="">

# Asset Browser

A fast browser for a folder of game assets on Windows, built with [Rune](https://github.com/anomalyco/rune).

- Thumbnails for textures (png, jpg, bmp, tga, webp), models (glb, gltf, obj, dae, fbx) and audio (wav, ogg, mp3, flac).
- Search names and folders, filter by pack, type and format. `-word` excludes.
- Orbit, pan and zoom models. Pan, zoom and tile textures. Scrub audio.
- Drag anything out to a folder or another program. Drags hand over a staged copy, so the library file is never touched.
- Drag a texture at a smaller size from the size ladder, or set a size for all grid drags.

```sh
cargo run --release
```

The library defaults to the dogfood folder. Use **Change library folder** in the app, or set `ASSET_BROWSER_ROOT`.

Thumbnails, the index and staged exports live in `%LOCALAPPDATA%\AssetBrowserV2`.

## Keys

Arrows move the selection. Space plays or pauses audio. `/` focuses search and Escape clears it.

## Checking it headlessly

```sh
cargo test
target/release/asset-browser --snap out.png --scene model --size 1520x900
```

Scenes: `textures`, `models`, `audio`, `search`, `small`, `texture`, `tile`, `tile-seams`, `big-texture`, `model`, `sound`. Set `AB_FRAME_MS=16` with a `--script` to scroll against the real thumbnail workers at 60 fps.
