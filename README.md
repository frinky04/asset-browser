# Asset Browser

Fast native Windows browser for a game-asset library (textures, GLB models, SFX) with
thumbnail grid, fuzzy search, 3D/audio preview, and **drag-and-drop out into any app**
(Godot, Blender, Explorer, ...).

Built with C++20, Dear ImGui, Win32 + DirectX 11. Dependencies (imgui, stb, cgltf,
miniaudio) are fetched automatically by CMake — no vcpkg needed.

## Build

```powershell
cmake -S . -B build -G "Visual Studio 17 2022" -A x64
cmake --build build --config Release
.\build\Release\asset-browser.exe
```

On first launch, Asset Browser opens without a folder selected. Use the in-app
**Choose asset folder...** button to select your library; Asset Browser remembers that
choice. Use **File → Change asset folder...** to switch later. For a one-off session,
override the saved folder with `asset-browser.exe --root "D:\SomeOtherFolder"`.

## Releases

Tagged releases include a self-contained 64-bit Windows executable and a SHA-256 checksum.
Windows 10 or newer with a Direct3D 11-capable GPU is recommended; no Visual C++ runtime
installation is required.

Maintainers can publish a release by pushing a semantic-version tag:

```powershell
git tag v0.1.0
git push origin v0.1.0
```

The release workflow can also be started manually from GitHub Actions with a
`vMAJOR.MINOR.PATCH` tag.

## Usage

- **Drag any tile into another program** to copy the file there (OBJ drags bring the MTL along).
- **Search**: type in the box (fuzzy, matches path + filename), `Ctrl+F` focuses it.
- **Filter**: type list (All/Textures/Models/Audio) + pack list in the left sidebar.
- **Models** are deduplicated: one tile per model, GLB preferred (the `+N` badge = other
  formats on disk). Right-click → *Drag models as* to hand over FBX/OBJ/DAE instead;
  right-click also offers *Copy path* for each format.
- **Audio**: click a tile to play/stop. In the preview panel, drag across the waveform to
  select a region (right-click to clear), then drag the
  "Drag out selection" button into another app to export it as a 16-bit WAV. With no
  selection it exports the whole clip (handy for OGG → WAV). Enable **Tune loop** to
  crossfade the end into the beginning and drag out a standalone WAV that repeats
  seamlessly; the crossfade length is adjustable. The single Play button and Space bar
  contextually play the tuned loop, selection, or full clip in that order.
- **Downres texture export**: right-click a texture → *Drag textures at* (half/quarter or
  a max pixel size). Drags then hand over a downscaled PNG instead of the original; the
  active mode shows in the status bar. Exported files land in
  `%LOCALAPPDATA%\AssetBrowser\export` (File → Open export folder), swept after 3 days.
- **Preview panel**: textures zoom (wheel) / pan (drag) at full resolution; GLB models
  orbit (drag) / dolly (wheel).
- **Double-click** a tile: reveal in Explorer.
- Tile size: slider in the status bar (or View menu).

## Thumbnails

Generated in background threads (GLB models are lit turntable renders; audio shows the
waveform) and cached in `%LOCALAPPDATA%\AssetBrowser\thumbs` keyed by path + mtime + size —
first browse of a folder is slower, afterwards it's instant. `File → Clear thumbnail cache`
wipes it; `File → Rescan assets` re-walks the asset root.

## Code map

| File | What |
|---|---|
| `src/main.cpp` | Win32 window, D3D11 device, frame loop, deferred OLE drag hook |
| `src/app.*` | App state, layout, sidebar, menus, selection, clipboard/Explorer actions |
| `src/assets.*` | Background scanner, junk filtering, model format dedupe, pack list |
| `src/search.cpp` | Fuzzy matcher + filter pipeline |
| `src/thumbs.*` | Worker pool, disk cache, upload budget, GLB bake scheduling |
| `src/model.*` | cgltf GLB loader, minimal lit renderer (thumbs + preview share it) |
| `src/audio.*` | miniaudio playback, WAV/OGG/MP3/FLAC decode, waveform rendering |
| `src/ui_grid.cpp` | Virtualized thumbnail grid, drag detection, context menu |
| `src/ui_preview.cpp` | Texture/model/audio preview panel |
| `src/dragdrop.*` | IDropSource + IDataObject (CF_HDROP) for OLE drag-out |
| `src/gfx.*` | D3D11 helpers, offscreen render targets |
