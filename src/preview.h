#pragma once
// Windowless previews: fitted texture, waveform + spectrogram, or rendered model views as RGBA8.
#include "assets.h"
#include "audio_analysis.h"
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
    // maxSide: textures are fitted into it, audio is a compact maxSide x maxSide*3/4 strip,
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

// A clip as one horizontal strip: header, dB-scaled waveform (peak + RMS), log-frequency
// spectrogram on a fixed -100..-20 dB colour scale, time axis. Segments are shaded and numbered.
struct AudioStripOptions {
    int width = 1200, height = 230;
    int index = 0;                 // 1-based number in the header; 0 = none
    std::string title, subtitle;   // bold name, dim context (pack/folder)
    std::string stats;             // right-aligned header text
    bool warn = false;             // stats in red (clipping)
    double spanSeconds = 0;        // > clip length: shared time scale, the clip is drawn narrower
    double timeOffset = 0;         // time-axis value of the first sample
    bool axes = true;              // dB/Hz gutter and time axis
    const std::vector<AudioSegment>* segments = nullptr;
    double markerAt = -1;          // axis time of a vertical marker line; < 0 = none
    std::string markerLabel;
};
RgbaImage RenderAudioStrip(const PcmData& pcm, const AudioStripOptions& o);

// Parts top to bottom, left-aligned, `gap` px apart.
RgbaImage StackVertical(const std::vector<RgbaImage>& parts, int gap);
