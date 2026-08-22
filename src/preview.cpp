#include "preview.h"
#include "audio.h"
#include "embed.h"
#include "util.h"
#include "stb_image.h"
#include "stb_image_resize2.h"
#include "stb_image_write.h"
#include <algorithm>
#include <cstring>

namespace {

bool FitTexture(const std::wstring& path, int maxSide, RgbaImage& out) {
    std::vector<uint8_t> bytes;
    if (!ReadFileBytes(path, bytes) || bytes.empty()) return false;
    int w = 0, h = 0, comp = 0;
    stbi_uc* px = stbi_load_from_memory(bytes.data(), (int)bytes.size(), &w, &h, &comp, 4);
    if (!px) return false;
    if (w <= maxSide && h <= maxSide) {
        out.w = w; out.h = h;
        out.px.assign(px, px + (size_t)w * h * 4);
    } else {
        float sc = std::min((float)maxSide / w, (float)maxSide / h);
        out.w = std::max(1, (int)(w * sc)); out.h = std::max(1, (int)(h * sc));
        out.px.resize((size_t)out.w * out.h * 4);
        stbir_resize_uint8_srgb(px, w, h, w * 4, out.px.data(), out.w, out.h, out.w * 4, STBIR_RGBA);
    }
    stbi_image_free(px);
    return true;
}

// 3x5 digit glyphs, row-major bits (MSB = left).
const uint8_t kDigits[10][5] = {
    {7,5,5,5,7},{2,6,2,2,7},{7,1,7,4,7},{7,1,7,1,7},{5,5,7,1,1},
    {7,4,7,1,7},{7,4,7,5,7},{7,1,1,1,1},{7,5,7,5,7},{7,5,7,1,7}};

void Fill(RgbaImage& img, int x0, int y0, int w, int h, uint8_t r, uint8_t g, uint8_t b) {
    for (int y = std::max(0, y0); y < std::min(img.h, y0 + h); ++y)
        for (int x = std::max(0, x0); x < std::min(img.w, x0 + w); ++x) {
            uint8_t* p = &img.px[((size_t)y * img.w + x) * 4];
            p[0] = r; p[1] = g; p[2] = b; p[3] = 255;
        }
}

void DrawNumber(RgbaImage& img, int x, int y, int n, int scale) {
    std::string s = std::to_string(n);
    int pad = scale, gw = 3 * scale, gh = 5 * scale;
    Fill(img, x, y, (int)s.size() * (gw + pad) + pad, gh + 2 * pad, 0, 0, 0);
    int cx = x + pad;
    for (char c : s) {
        const uint8_t* g = kDigits[c - '0'];
        for (int row = 0; row < 5; ++row)
            for (int col = 0; col < 3; ++col)
                if (g[row] & (4 >> col)) Fill(img, cx + col * scale, y + pad + row * scale, scale, scale, 255, 255, 255);
        cx += gw + pad;
    }
}

} // namespace

PreviewRenderer::~PreviewRenderer() {
    if (gpuState == 1) { rt.Release(); renderer.Shutdown(); gfx.Shutdown(); }
}

bool PreviewRenderer::EnsureGpu(std::string& err) {
    if (gpuState == 1) return true;
    if (gpuState == -1) { err = "no Direct3D 11 device available for model rendering"; return false; }
    gpuState = (gfx.InitHeadless() && renderer.Init(gfx)) ? 1 : -1;
    if (gpuState == -1) { gfx.Shutdown(); err = "no Direct3D 11 device available for model rendering"; }
    return gpuState == 1;
}

bool PreviewRenderer::Make(const AssetEntry& e, int maxSide, bool sheet, RgbaImage& out, std::string& err) {
    out = {};
    if (e.type == AssetType::Texture) {
        if (!FitTexture(e.path, maxSide, out)) { err = "cannot decode texture"; return false; }
        return true;
    }
    if (e.type == AssetType::Audio) {
        PcmData pcm = DecodeAudioFile(e.path);
        if (!pcm.valid) { err = "cannot decode audio"; return false; }
        out.w = maxSide; out.h = std::max(16, maxSide / 3);
        out.px = RenderWaveformThumb(pcm, out.w, out.h);
        return out.valid();
    }
    if (!EmbedModelRenderable(e.ext)) { err = "no renderer for " + e.ext; return false; }
    if (!EnsureGpu(err)) return false;
    std::wstring lower = ToLowerAsciiW(e.path);
    bool gltf = lower.ends_with(L".glb") || lower.ends_with(L".gltf");
    CpuModel model = gltf ? LoadGlbCpu(e.path) : LoadFbxObjCpu(e.path);
    if (!model.valid) { err = "cannot parse model"; return false; }
    int side = sheet ? kEmbedModelView : maxSide;
    if (rt.w != side || rt.h != side) rt.Create(gfx.device, side, side, /*withStaging*/ true);
    bool ok = sheet ? RenderModelSheet(gfx, renderer, rt, model, out.px)
                    : RenderModelHero(gfx, renderer, rt, model, out.px);
    if (!ok) { err = "model render failed"; return false; }
    out.w = sheet ? side * kEmbedModelSheetCols : side;
    out.h = sheet ? side * kEmbedModelSheetRows : side;
    return out.valid();
}

RgbaImage ComposeContactSheet(const std::vector<RgbaImage>& tiles, int cell, int cols) {
    RgbaImage sheet;
    if (tiles.empty()) return sheet;
    cols = std::max(1, std::min(cols, (int)tiles.size()));
    int rows = ((int)tiles.size() + cols - 1) / cols;
    sheet.w = cols * cell; sheet.h = rows * cell;
    sheet.px.assign((size_t)sheet.w * sheet.h * 4, 0);
    Fill(sheet, 0, 0, sheet.w, sheet.h, 40, 40, 44);
    for (size_t i = 0; i < tiles.size(); ++i) {
        int cx = (int)(i % cols) * cell, cy = (int)(i / cols) * cell;
        const RgbaImage& t = tiles[i];
        if (t.valid()) {
            // centre, downscale if the tile is bigger than the cell
            RgbaImage fitted;
            const RgbaImage* src = &t;
            if (t.w > cell || t.h > cell) {
                float sc = std::min((float)cell / t.w, (float)cell / t.h);
                fitted.w = std::max(1, (int)(t.w * sc)); fitted.h = std::max(1, (int)(t.h * sc));
                fitted.px.resize((size_t)fitted.w * fitted.h * 4);
                stbir_resize_uint8_srgb(t.px.data(), t.w, t.h, t.w * 4, fitted.px.data(), fitted.w, fitted.h, fitted.w * 4, STBIR_RGBA);
                src = &fitted;
            }
            int ox = cx + (cell - src->w) / 2, oy = cy + (cell - src->h) / 2;
            for (int y = 0; y < src->h; ++y)
                memcpy(&sheet.px[((size_t)(oy + y) * sheet.w + ox) * 4], &src->px[(size_t)y * src->w * 4], (size_t)src->w * 4);
        } else {
            Fill(sheet, cx + 4, cy + 4, cell - 8, cell - 8, 90, 40, 40); // unrenderable
        }
        DrawNumber(sheet, cx + 4, cy + 4, (int)i + 1, std::max(2, cell / 48));
    }
    return sheet;
}

bool WritePng(const std::wstring& path, const RgbaImage& img) {
    return img.valid() && stbi_write_png(WideToUtf8(path).c_str(), img.w, img.h, 4, img.px.data(), img.w * 4) != 0;
}
