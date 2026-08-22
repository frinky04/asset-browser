#include "gfx.h"

bool Gfx::Init(HWND hwnd) {
    DXGI_SWAP_CHAIN_DESC sd{};
    sd.BufferCount = 2;
    sd.BufferDesc.Format = DXGI_FORMAT_R8G8B8A8_UNORM;
    sd.BufferUsage = DXGI_USAGE_RENDER_TARGET_OUTPUT;
    sd.OutputWindow = hwnd;
    sd.SampleDesc.Count = 1;
    sd.Windowed = TRUE;
    sd.SwapEffect = DXGI_SWAP_EFFECT_FLIP_DISCARD;

    UINT flags = 0;
    D3D_FEATURE_LEVEL levels[] = { D3D_FEATURE_LEVEL_11_0 };
    HRESULT hr = D3D11CreateDeviceAndSwapChain(
        nullptr, D3D_DRIVER_TYPE_HARDWARE, nullptr, flags, levels, 1,
        D3D11_SDK_VERSION, &sd, &swap, &device, nullptr, &ctx);
    if (FAILED(hr)) {
        hr = D3D11CreateDeviceAndSwapChain(
            nullptr, D3D_DRIVER_TYPE_WARP, nullptr, 0, levels, 1,
            D3D11_SDK_VERSION, &sd, &swap, &device, nullptr, &ctx);
    }
    if (FAILED(hr)) return false;
    CreateBackbufferRTV();

    D3D11_SAMPLER_DESC samp{};
    samp.Filter = D3D11_FILTER_MIN_MAG_MIP_LINEAR;
    samp.AddressU = samp.AddressV = samp.AddressW = D3D11_TEXTURE_ADDRESS_WRAP;
    samp.MaxLOD = D3D11_FLOAT32_MAX;
    device->CreateSamplerState(&samp, &wrapSampler);
    return true;
}

bool Gfx::InitHeadless() {
    D3D_FEATURE_LEVEL levels[] = { D3D_FEATURE_LEVEL_11_0 };
    HRESULT hr = D3D11CreateDevice(nullptr, D3D_DRIVER_TYPE_HARDWARE, nullptr, 0, levels, 1,
                                   D3D11_SDK_VERSION, &device, nullptr, &ctx);
    if (FAILED(hr))
        hr = D3D11CreateDevice(nullptr, D3D_DRIVER_TYPE_WARP, nullptr, 0, levels, 1,
                               D3D11_SDK_VERSION, &device, nullptr, &ctx);
    if (FAILED(hr)) return false;
    D3D11_SAMPLER_DESC samp{};
    samp.Filter = D3D11_FILTER_MIN_MAG_MIP_LINEAR;
    samp.AddressU = samp.AddressV = samp.AddressW = D3D11_TEXTURE_ADDRESS_WRAP;
    samp.MaxLOD = D3D11_FLOAT32_MAX;
    device->CreateSamplerState(&samp, &wrapSampler);
    return true;
}

void Gfx::Shutdown() {
    SafeRelease(wrapSampler);
    SafeRelease(backbufferRTV);
    SafeRelease(swap);
    SafeRelease(ctx);
    SafeRelease(device);
}

void Gfx::CreateBackbufferRTV() {
    ID3D11Texture2D* bb = nullptr;
    if (SUCCEEDED(swap->GetBuffer(0, IID_PPV_ARGS(&bb)))) {
        device->CreateRenderTargetView(bb, nullptr, &backbufferRTV);
        D3D11_TEXTURE2D_DESC d; bb->GetDesc(&d);
        width = (int)d.Width; height = (int)d.Height;
        bb->Release();
    }
}

void Gfx::Resize(int w, int h) {
    if (!swap || w <= 0 || h <= 0) return;
    SafeRelease(backbufferRTV);
    swap->ResizeBuffers(0, (UINT)w, (UINT)h, DXGI_FORMAT_UNKNOWN, 0);
    CreateBackbufferRTV();
}

ID3D11ShaderResourceView* Gfx::CreateTextureSRV(const uint8_t* rgba, int w, int h, bool srgb) {
    if (!rgba || w <= 0 || h <= 0) return nullptr;
    D3D11_TEXTURE2D_DESC td{};
    td.Width = (UINT)w; td.Height = (UINT)h;
    td.MipLevels = 1; td.ArraySize = 1;
    td.Format = srgb ? DXGI_FORMAT_R8G8B8A8_UNORM_SRGB : DXGI_FORMAT_R8G8B8A8_UNORM;
    td.SampleDesc.Count = 1;
    td.Usage = D3D11_USAGE_IMMUTABLE;
    td.BindFlags = D3D11_BIND_SHADER_RESOURCE;
    D3D11_SUBRESOURCE_DATA init{ rgba, (UINT)(w * 4), 0 };
    ID3D11Texture2D* tex = nullptr;
    if (FAILED(device->CreateTexture2D(&td, &init, &tex))) return nullptr;
    ID3D11ShaderResourceView* srv = nullptr;
    device->CreateShaderResourceView(tex, nullptr, &srv);
    tex->Release();
    return srv;
}

bool OffscreenTarget::Create(ID3D11Device* dev, int w_, int h_, bool withStaging) {
    Release();
    w = w_; h = h_;
    D3D11_TEXTURE2D_DESC td{};
    td.Width = (UINT)w; td.Height = (UINT)h;
    td.MipLevels = 1; td.ArraySize = 1;
    td.Format = DXGI_FORMAT_R8G8B8A8_TYPELESS;
    td.SampleDesc.Count = 1;
    td.Usage = D3D11_USAGE_DEFAULT;
    td.BindFlags = D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE;
    if (FAILED(dev->CreateTexture2D(&td, nullptr, &tex))) return false;

    D3D11_RENDER_TARGET_VIEW_DESC rd{};
    rd.Format = DXGI_FORMAT_R8G8B8A8_UNORM_SRGB;
    rd.ViewDimension = D3D11_RTV_DIMENSION_TEXTURE2D;
    if (FAILED(dev->CreateRenderTargetView(tex, &rd, &rtv))) return false;

    D3D11_SHADER_RESOURCE_VIEW_DESC sd{};
    sd.Format = DXGI_FORMAT_R8G8B8A8_UNORM;
    sd.ViewDimension = D3D11_SRV_DIMENSION_TEXTURE2D;
    sd.Texture2D.MipLevels = 1;
    if (FAILED(dev->CreateShaderResourceView(tex, &sd, &srv))) return false;

    D3D11_TEXTURE2D_DESC dd = td;
    dd.Format = DXGI_FORMAT_D24_UNORM_S8_UINT;
    dd.BindFlags = D3D11_BIND_DEPTH_STENCIL;
    if (FAILED(dev->CreateTexture2D(&dd, nullptr, &depth))) return false;
    if (FAILED(dev->CreateDepthStencilView(depth, nullptr, &dsv))) return false;

    if (withStaging) {
        D3D11_TEXTURE2D_DESC sdd = td;
        sdd.Format = DXGI_FORMAT_R8G8B8A8_UNORM;
        sdd.Usage = D3D11_USAGE_STAGING;
        sdd.BindFlags = 0;
        sdd.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
        if (FAILED(dev->CreateTexture2D(&sdd, nullptr, &staging))) return false;
    }
    return true;
}

void OffscreenTarget::Release() {
    SafeRelease(staging);
    SafeRelease(dsv);
    SafeRelease(depth);
    SafeRelease(srv);
    SafeRelease(rtv);
    SafeRelease(tex);
    w = h = 0;
}

bool OffscreenTarget::ReadPixels(ID3D11DeviceContext* ctx, std::vector<uint8_t>& out) {
    if (!staging || !tex) return false;
    ctx->CopyResource(staging, tex);
    D3D11_MAPPED_SUBRESOURCE map{};
    if (FAILED(ctx->Map(staging, 0, D3D11_MAP_READ, 0, &map))) return false;
    out.resize((size_t)w * h * 4);
    for (int y = 0; y < h; ++y)
        memcpy(out.data() + (size_t)y * w * 4, (uint8_t*)map.pData + (size_t)y * map.RowPitch, (size_t)w * 4);
    ctx->Unmap(staging, 0);
    return true;
}
