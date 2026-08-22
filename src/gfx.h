#pragma once
#include "util.h"
#include <d3d11.h>
#include <vector>
#include <cstdint>

struct Gfx {
    ID3D11Device*           device = nullptr;
    ID3D11DeviceContext*    ctx = nullptr;
    IDXGISwapChain*         swap = nullptr;
    ID3D11RenderTargetView* backbufferRTV = nullptr;
    ID3D11SamplerState*     wrapSampler = nullptr; // linear+wrap, for tiled texture preview
    int width = 0, height = 0;

    bool Init(HWND hwnd);
    bool InitHeadless();   // device + context only, no window or swap chain
    void Shutdown();
    void Resize(int w, int h);

    // Upload RGBA8 pixels -> SRV for display (srgb=true creates an sRGB view, used
    // for model textures sampled during lit rendering; thumbnails for ImGui display
    // use srgb=false so bytes pass through unchanged).
    ID3D11ShaderResourceView* CreateTextureSRV(const uint8_t* rgba, int w, int h, bool srgb);

private:
    void CreateBackbufferRTV();
};

// Offscreen render target used for GLB thumbnail bakes and the 3D preview panel.
// Texture is TYPELESS: RTV is sRGB (correct lit rendering), SRV is UNORM
// (pass-through display via ImGui).
struct OffscreenTarget {
    ID3D11Texture2D*          tex = nullptr;
    ID3D11RenderTargetView*   rtv = nullptr;
    ID3D11ShaderResourceView* srv = nullptr;
    ID3D11Texture2D*          depth = nullptr;
    ID3D11DepthStencilView*   dsv = nullptr;
    ID3D11Texture2D*          staging = nullptr;
    int w = 0, h = 0;

    bool Create(ID3D11Device* dev, int w, int h, bool withStaging);
    void Release();
    bool ReadPixels(ID3D11DeviceContext* ctx, std::vector<uint8_t>& outRgba);
};

template <typename T> inline void SafeRelease(T*& p) { if (p) { p->Release(); p = nullptr; } }
