# Asset Browser

A fast native Windows browser for large game-asset libraries. Search, filter, preview,
and drag textures, models, and audio directly into tools such as Godot, Blender, and
Explorer.

## What it does

- Fuzzy-searches filenames and paths, with filters for asset type and top-level pack.
- Previews textures, GLB models, and audio without opening another application.
- Drags original files or generated variants into any application that accepts files.
- Deduplicates equivalent model formats into one entry while preserving format choice.
- Exports resized textures, audio selections, and seamless audio loops on demand.
- Generates thumbnails in background workers and caches them for fast subsequent loads.

Supported formats:

- Textures: PNG, JPG, BMP, TGA, GIF, PSD
- Models: GLB, glTF, FBX, OBJ, DAE
- Audio: WAV, OGG, MP3, FLAC

## Use

Launch the app, choose an asset folder, then search or filter the resulting grid. Click an
asset to preview it, double-click to reveal it in Explorer, or drag it into another app.
Right-click assets for alternate model formats and export options.

The selected folder is remembered and can be changed from **File → Change asset folder**.
Use `asset-browser.exe --root "D:\Assets"` for a one-off override.

Asset Browser scans recursively, ignores unrelated file types, and stores only settings,
thumbnail caches, and temporary exports under `%LOCALAPPDATA%\AssetBrowser`.

## Build

Requires Windows 10+, CMake, and Visual Studio 2022. Dependencies are fetched by CMake.

```powershell
cmake -S . -B build -G "Visual Studio 17 2022" -A x64
cmake --build build --config Release
.\build\Release\asset-browser.exe
```

Built with C++20, Dear ImGui, Win32, and Direct3D 11.

## Release

Push a `vMAJOR.MINOR.PATCH` tag, or run the Release workflow manually. GitHub Actions
publishes a self-contained Windows x64 zip with a SHA-256 checksum.
