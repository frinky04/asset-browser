#pragma once
// Windowless previews: fitted texture, waveform, or rendered model views as RGBA8.
#include "assets.h"
#include "gfx.h"
#include "model.h"
#include <cstdint>
#include <string>
#include <vector>

struct RgbaImage {
    int w = 0, h = 0;
    std::vector<uint8_t> px;
    std::string label;   // short caption burned into contact sheets ("512x512", "1.8s", "2.0k")
    bool valid() const { return w > 0 && h > 0 && px.size() == (size_t)w * h * 4; }
};

// Lazily creates a D3D device and model renderer the first time a model is drawn.
struct PreviewRenderer {
    ~PreviewRenderer();
    // maxSide: textures are fitted into it, audio waveforms are maxSide x maxSide/3,
    // models render a hero view at maxSide square (or the 3x2 sheet when `sheet`).
    bool Make(const AssetEntry& e, int maxSide, bool sheet, RgbaImage& out, std::string& err);
private:
    bool EnsureGpu(std::string& err);
    Gfx gfx;
    ModelRenderer renderer;
    OffscreenTarget rt;
    int gpuState = 0; // 0 untried, 1 ok, -1 failed
};

// Tile images left-to-right, top-to-bottom into a grid of `cell` px cells and
// burn a 1-based index into each tile's corner.
RgbaImage ComposeContactSheet(const std::vector<RgbaImage>& tiles, int cell, int cols);

bool WritePng(const std::wstring& path, const RgbaImage& img);
