#include "preview.h"
#include "audio.h"
#include "embed.h"
#include "util.h"
#include "stb_image.h"
#include "stb_image_resize2.h"
#include "stb_image_write.h"
#include "audio_analysis.h"
#include <algorithm>
#include <cmath>
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

struct Rgb { uint8_t r, g, b; };

void Fill(RgbaImage& img, int x0, int y0, int w, int h, uint8_t r, uint8_t g, uint8_t b) {
    for (int y = std::max(0, y0); y < std::min(img.h, y0 + h); ++y)
        for (int x = std::max(0, x0); x < std::min(img.w, x0 + w); ++x) {
            uint8_t* p = &img.px[((size_t)y * img.w + x) * 4];
            p[0] = r; p[1] = g; p[2] = b; p[3] = 255;
        }
}

void Blend(RgbaImage& img, int x, int y, Rgb c, float a) {
    if (x < 0 || y < 0 || x >= img.w || y >= img.h || a <= 0) return;
    uint8_t* p = &img.px[((size_t)y * img.w + x) * 4];
    p[0] = (uint8_t)(p[0] + (c.r - p[0]) * a);
    p[1] = (uint8_t)(p[1] + (c.g - p[1]) * a);
    p[2] = (uint8_t)(p[2] + (c.b - p[2]) * a);
    p[3] = 255;
}

void BlendRect(RgbaImage& img, int x0, int y0, int w, int h, Rgb c, float a) {
    for (int y = y0; y < y0 + h; ++y)
        for (int x = x0; x < x0 + w; ++x) Blend(img, x, y, c, a);
}

// Text through GDI into a DIB, used as coverage: real fonts and any Unicode file name,
// with no font files to ship. Returns the drawn width.
int PutText(RgbaImage* img, int x, int y, const std::string& s, int px, bool bold, Rgb c, int* height = nullptr) {
    std::wstring w = Utf8ToWide(s);
    if (w.empty()) return 0;
    HDC dc = CreateCompatibleDC(nullptr);
    HFONT font = CreateFontW(-px, 0, 0, 0, bold ? FW_SEMIBOLD : FW_NORMAL, 0, 0, 0, DEFAULT_CHARSET,
                             OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, ANTIALIASED_QUALITY, DEFAULT_PITCH, L"Segoe UI");
    HGDIOBJ oldFont = SelectObject(dc, font);
    SIZE sz{};
    GetTextExtentPoint32W(dc, w.c_str(), (int)w.size(), &sz);
    if (height) *height = sz.cy;
    if (img && sz.cx > 0 && sz.cy > 0) {
        BITMAPINFO bi{};
        bi.bmiHeader.biSize = sizeof(bi.bmiHeader);
        bi.bmiHeader.biWidth = sz.cx; bi.bmiHeader.biHeight = -sz.cy;
        bi.bmiHeader.biPlanes = 1; bi.bmiHeader.biBitCount = 32; bi.bmiHeader.biCompression = BI_RGB;
        void* bits = nullptr;
        HBITMAP bmp = CreateDIBSection(dc, &bi, DIB_RGB_COLORS, &bits, nullptr, 0);
        if (bmp && bits) {
            HGDIOBJ oldBmp = SelectObject(dc, bmp);
            SetBkMode(dc, TRANSPARENT);
            SetTextColor(dc, RGB(255, 255, 255));
            TextOutW(dc, 0, 0, w.c_str(), (int)w.size());
            GdiFlush();
            const uint8_t* cov = (const uint8_t*)bits;
            for (int yy = 0; yy < sz.cy; ++yy)
                for (int xx = 0; xx < sz.cx; ++xx)
                    Blend(*img, x + xx, y + yy, c, cov[((size_t)yy * sz.cx + xx) * 4 + 1] / 255.0f);
            SelectObject(dc, oldBmp);
        }
        if (bmp) DeleteObject(bmp);
    }
    SelectObject(dc, oldFont);
    DeleteObject(font);
    DeleteDC(dc);
    return sz.cx;
}

int TextWidth(const std::string& s, int px, bool bold) { return PutText(nullptr, 0, 0, s, px, bold, {}); }

// Text on a solid box; returns the box width.
int PutBoxText(RgbaImage& img, int x, int y, const std::string& s, int px, bool bold, Rgb fg, Rgb bg) {
    int th = 0, tw = PutText(nullptr, 0, 0, s, px, bold, fg, &th);
    int pad = std::max(2, px / 4);
    Fill(img, x, y, tw + 2 * pad, th, bg.r, bg.g, bg.b);
    PutText(&img, x + pad, y, s, px, bold, fg);
    return tw + 2 * pad;
}

void DrawLabel(RgbaImage& img, int x, int y, const std::string& s, int scale) {
    PutBoxText(img, x, y, s, 6 * scale + 2, true, {255, 255, 255}, {0, 0, 0});
}

// matplotlib "magma", 9 stops
Rgb Magma(float t) {
    static const Rgb stops[9] = {{0, 0, 4}, {28, 16, 68}, {79, 18, 123}, {129, 37, 129}, {181, 54, 122},
                                 {229, 80, 100}, {251, 135, 97}, {254, 194, 135}, {252, 253, 191}};
    t = std::clamp(t, 0.0f, 1.0f) * 8.0f;
    int i = std::min(7, (int)t);
    float f = t - i;
    const Rgb &a = stops[i], &b = stops[i + 1];
    return {(uint8_t)(a.r + (b.r - a.r) * f), (uint8_t)(a.g + (b.g - a.g) * f), (uint8_t)(a.b + (b.b - a.b) * f)};
}

std::string TimeLabel(double t, double step) {
    char buf[32];
    int dec = step >= 1 ? 0 : step >= 0.1 ? 1 : step >= 0.01 ? 2 : 3;
    if (std::fabs(t) < step * 1e-3) t = 0;
    snprintf(buf, sizeof(buf), "%.*fs", dec, t);
    return buf;
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
        std::vector<uint8_t> bytes; int w = 0, h = 0, comp = 0;
        if (ReadFileBytes(e.path, bytes) && stbi_info_from_memory(bytes.data(), (int)bytes.size(), &w, &h, &comp))
            out.label = std::to_string(w) + "x" + std::to_string(h);
        if (!FitTexture(e.path, maxSide, out)) { err = "cannot decode texture"; return false; }
        return true;
    }
    if (e.type == AssetType::Audio) {
        PcmData pcm = DecodeAudioFile(e.path);
        if (!pcm.valid) { err = "cannot decode audio"; return false; }
        AudioStripOptions so;
        so.width = maxSide; so.height = std::max(32, maxSide * 3 / 4);
        so.axes = false;
        out = RenderAudioStrip(pcm, so);
        char sec[32]; snprintf(sec, sizeof(sec), "%.1fs", (double)pcm.frames / pcm.sampleRate);
        out.label = sec;
        return out.valid();
    }
    if (!EmbedModelRenderable(e.ext)) { err = "no renderer for " + e.ext; return false; }
    if (!EnsureGpu(err)) return false;
    std::wstring lower = ToLowerAsciiW(e.path);
    bool gltf = lower.ends_with(L".glb") || lower.ends_with(L".gltf");
    CpuModel model = gltf ? LoadGlbCpu(e.path) : LoadFbxObjCpu(e.path);
    if (!model.valid) { err = "cannot parse model"; return false; }
    {
        size_t tris = model.indices.size() / 3;
        char t[32];
        if (tris >= 1000) snprintf(t, sizeof(t), "%.1fk", tris / 1000.0); else snprintf(t, sizeof(t), "%zu", tris);
        out.label = t;
    }
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
        int scale = std::max(2, cell / 48);
        DrawLabel(sheet, cx + 4, cy + 4, std::to_string(i + 1), scale);
        if (!t.label.empty()) DrawLabel(sheet, cx + 4, cy + cell - 7 * scale - 4, t.label, scale);
    }
    return sheet;
}

bool WritePng(const std::wstring& path, const RgbaImage& img) {
    return img.valid() && stbi_write_png(WideToUtf8(path).c_str(), img.w, img.h, 4, img.px.data(), img.w * 4) != 0;
}

RgbaImage RenderAudioStrip(const PcmData& pcm, const AudioStripOptions& o) {
    RgbaImage img;
    img.w = o.width; img.h = o.height;
    img.px.assign((size_t)img.w * img.h * 4, 0);
    Fill(img, 0, 0, img.w, img.h, 22, 22, 26);
    if (!pcm.valid || pcm.frames == 0 || pcm.sampleRate == 0) return img;

    const double clipSec = (double)pcm.frames / pcm.sampleRate;
    const double span = std::max(o.spanSeconds, clipSec);
    const bool header = o.index > 0 || !o.title.empty() || !o.stats.empty();
    const int headerH = header ? 22 : 0, axisH = o.axes ? 16 : 0;
    const int gutter = o.axes ? 40 : 0, rightPad = o.axes ? 8 : 0;
    const int bodyY = headerH, bodyH = img.h - headerH - axisH;
    const int waveH = std::max(8, (int)(bodyH * 0.36f)), specY = bodyY + waveH + 2, specH = bodyH - waveH - 2;
    const int plotX = gutter, plotW = img.w - gutter - rightPad;
    if (specH < 8 || plotW < 16) return img;
    const int clipW = std::clamp((int)std::lround(plotW * clipSec / span), 1, plotW);
    const Rgb dim{140, 142, 152};
    const int labelPx = 11;
    auto xAt = [&](double sec) { return plotX + (int)std::lround(sec / span * plotW); }; // clip time

    // ---- header: [index] title  subtitle ............ stats
    if (header) {
        int x = 6;
        if (o.index > 0) x += PutBoxText(img, x, 2, std::to_string(o.index), 15, true, {255, 255, 255}, {58, 92, 160}) + 8;
        int statsW = TextWidth(o.stats, 13, false);
        int statsX = img.w - rightPad - 4 - statsW;
        PutText(&img, statsX, 3, o.stats, 13, false, o.warn ? Rgb{255, 128, 112} : Rgb{205, 208, 218});
        int room = statsX - x - 12;
        std::string title = o.title;
        if (TextWidth(title, 14, true) > room) {
            // drop whole UTF-8 characters from the end until it fits with an ellipsis
            while (!title.empty() && TextWidth(title + "\xE2\x80\xA6", 14, true) > room) {
                do title.pop_back(); while (!title.empty() && ((uint8_t)title.back() & 0xC0) == 0x80);
            }
            title += "\xE2\x80\xA6";
        }
        x += PutText(&img, x, 2, title, 14, true, {236, 236, 242}) + 8;
        if (!o.subtitle.empty() && x + TextWidth(o.subtitle, 12, false) < statsX - 12)
            PutText(&img, x, 4, o.subtitle, 12, false, dim);
    }

    // ---- waveform panel, dB-scaled so quiet tails stay visible
    Fill(img, plotX, bodyY, plotW, waveH, 16, 16, 19);
    Fill(img, plotX, bodyY, clipW, waveH, 30, 31, 37);
    const std::vector<AudioSegment>* segs = o.segments;
    if (segs)
        for (size_t k = 0; k < segs->size(); ++k) {
            int x0 = xAt((*segs)[k].start), x1 = std::max(x0 + 1, xAt((*segs)[k].end));
            BlendRect(img, x0, bodyY, x1 - x0, waveH, k % 2 ? Rgb{120, 90, 190} : Rgb{60, 130, 200}, 0.22f);
        }
    const int mid = bodyY + waveH / 2, half = waveH / 2 - 1;
    auto yOff = [&](double db) { return (int)std::lround(std::clamp((db + 60.0) / 60.0, 0.0, 1.0) * half); };
    for (double db : {-20.0, -40.0}) // dB grid, behind the wave
        for (int x = plotX; x < plotX + clipW; x += 3) {
            Blend(img, x, mid - yOff(db), {255, 255, 255}, 0.10f);
            Blend(img, x, mid + yOff(db), {255, 255, 255}, 0.10f);
        }
    std::vector<float> pk, rms;
    EnvelopeDb(pcm, clipW, pk, rms);
    for (int i = 0; i < clipW; ++i) {
        int x = plotX + i, p = yOff(pk[i]), r = yOff(rms[i]);
        for (int y = mid - p; y <= mid + p; ++y) Blend(img, x, y, {100, 118, 150}, 1.0f);
        for (int y = mid - r; y <= mid + r; ++y) Blend(img, x, y, {200, 210, 228}, 1.0f);
        if (pk[i] >= -0.01f) { // clipping
            Fill(img, x, bodyY, 1, 3, 255, 70, 60);
            Fill(img, x, bodyY + waveH - 3, 1, 3, 255, 70, 60);
        }
    }

    // ---- spectrogram, fixed dB scale so strips compare
    Fill(img, plotX, specY, plotW, specH, 10, 10, 12);
    const float loHz = 40.0f, hiHz = std::min(20000.0f, pcm.sampleRate / 2.0f);
    std::vector<float> spec = SpectrogramDb(pcm, clipW, specH, loHz, hiHz);
    const float dbLo = -100.0f, dbHi = -20.0f;
    for (int y = 0; y < specH; ++y)
        for (int i = 0; i < clipW; ++i) {
            Rgb c = Magma((spec[(size_t)y * clipW + i] - dbLo) / (dbHi - dbLo));
            uint8_t* p = &img.px[((size_t)(specY + y) * img.w + plotX + i) * 4];
            p[0] = c.r; p[1] = c.g; p[2] = c.b; p[3] = 255;
        }
    auto yHz = [&](double hz) { return specY + (int)std::lround((1.0 - std::log(hz / loHz) / std::log(hiHz / loHz)) * specH); };
    for (double hz : {100.0, 1000.0, 10000.0}) {
        if (hz >= hiHz) continue;
        int y = yHz(hz);
        for (int x = plotX; x < plotX + clipW; x += 4) Blend(img, x, y, {255, 255, 255}, 0.22f);
    }
    if (segs)
        for (const AudioSegment& sg : *segs)
            for (int y = specY; y < specY + specH; y += 2) Blend(img, xAt(sg.start), y, {130, 220, 255}, 0.55f);

    // segment numbers on the waveform, when they fit
    if (segs && segs->size() > 1)
        for (size_t k = 0; k < segs->size(); ++k) {
            std::string n = std::to_string(k + 1);
            int x0 = xAt((*segs)[k].start), x1 = xAt((*segs)[k].end);
            if (TextWidth(n, labelPx, true) + 4 > x1 - x0 + 2) continue;
            PutBoxText(img, x0 + 1, bodyY + 1, n, labelPx, true, {255, 255, 255}, {30, 70, 120});
        }

    if (o.markerAt >= 0) {
        int x = plotX + (int)std::lround((o.markerAt - o.timeOffset) / span * plotW);
        Fill(img, x, bodyY, 1, bodyH, 255, 214, 90);
        if (!o.markerLabel.empty()) PutBoxText(img, x + 3, bodyY + 1, o.markerLabel, labelPx, true, {20, 20, 20}, {255, 214, 90});
    }

    if (!o.axes) return img;

    // ---- gutter labels: dB for the waveform, Hz for the spectrogram
    auto gutterLabel = [&](const std::string& s, int y) {
        int th = 0, tw = PutText(nullptr, 0, 0, s, labelPx, false, dim, &th);
        PutText(&img, gutter - 5 - tw, std::clamp(y - th / 2, 0, img.h - th), s, labelPx, false, dim);
    };
    if (waveH >= 110) {
        gutterLabel("0 dB", bodyY + 6);
        gutterLabel("-20", mid - yOff(-20));
        gutterLabel("-40", mid - yOff(-40));
    } else {
        gutterLabel("dB", mid);
    }
    for (auto [hz, s] : {std::pair{100.0, "100"}, {1000.0, "1k"}, {10000.0, "10k"}})
        if (hz < hiHz) gutterLabel(s, yHz(hz));

    // ---- time axis
    const int axisY = specY + specH;
    double raw = span / std::max(2, plotW / 90);
    double p10 = std::pow(10.0, std::floor(std::log10(raw))), step = p10 * 10;
    for (double m : {1.0, 2.0, 5.0}) if (m * p10 >= raw) { step = m * p10; break; }
    for (double t = std::ceil(o.timeOffset / step - 1e-9) * step; t <= o.timeOffset + span + 1e-9; t += step) {
        int x = plotX + (int)std::lround((t - o.timeOffset) / span * plotW);
        Fill(img, x, axisY, 1, 4, 150, 150, 160);
        for (int y = bodyY; y < bodyY + waveH; y += 3) Blend(img, x, y, {255, 255, 255}, 0.12f);
        std::string s = TimeLabel(t, step);
        int tw = TextWidth(s, labelPx, false);
        PutText(&img, std::clamp(x - tw / 2, 0, img.w - tw), axisY + 2, s, labelPx, false, dim);
    }
    return img;
}

RgbaImage StackVertical(const std::vector<RgbaImage>& parts, int gap) {
    RgbaImage out;
    for (const RgbaImage& p : parts) { out.w = std::max(out.w, p.w); out.h += p.h; }
    if (parts.empty() || out.w == 0) return out;
    out.h += gap * ((int)parts.size() - 1);
    out.px.assign((size_t)out.w * out.h * 4, 0);
    Fill(out, 0, 0, out.w, out.h, 8, 8, 10);
    int y = 0;
    for (const RgbaImage& p : parts) {
        if (p.valid())
            for (int r = 0; r < p.h; ++r)
                memcpy(&out.px[((size_t)(y + r) * out.w) * 4], &p.px[(size_t)r * p.w * 4], (size_t)p.w * 4);
        y += p.h + gap;
    }
    return out;
}
