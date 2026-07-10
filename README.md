# Asset Browser

Fast native Windows browser for a game-asset library (textures, GLB models, SFX) with
thumbnail grid, fuzzy search, 3D/audio preview, and **drag-and-drop out into any app**
(Godot, Blender, Explorer, ...).

Built with C++20, Dear ImGui, Win32 + DirectX 11. Dependencies (imgui, stb, cgltf,
miniaudio) are fetched automatically by CMake — no vcpkg needed.

## Build

```powershell
cmake -S . -B build -G "Visual Studio 18 2026" -A x64
cmake --build build --config Release
.\build\Release\asset-browser.exe
```

Default asset root is `C:\Users\cuddl\Documents\Dev\Games\Assets`; override with
`asset-browser.exe --root "D:\SomeOtherFolder"`.

## Usage

- **Drag any tile into another program** to copy the file there (OBJ drags bring the MTL along).
- **Search**: type in the box (fuzzy, matches path + filename), `Ctrl+F` focuses it.
- **Filter**: type list (All/Textures/Models/Audio) + pack list in the left sidebar.
- **Models** are deduplicated: one tile per model, GLB preferred (the `+N` badge = other
  formats on disk). Right-click → *Drag models as* to hand over FBX/OBJ/DAE instead;
  right-click also offers *Copy path* for each format.
- **Audio**: click a tile to play/stop. In the preview panel, drag across the waveform to
  select a region ("Play selection" to audition, right-click to clear), then drag the
  "Drag out selection" button into another app to export it as a 16-bit WAV. With no
  selection it exports the whole clip (handy for OGG → WAV).
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
