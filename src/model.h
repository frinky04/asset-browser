#pragma once
#include "gfx.h"
#include <vector>
#include <string>
#include <cstdint>

// 12 floats: pos(3) normal(3) uv(2) color(4)
struct CpuVertex { float px, py, pz, nx, ny, nz, u, v, r, g, b, a; };

struct CpuImage { int w = 0, h = 0; std::vector<uint8_t> rgba; };

struct CpuPrim {
    uint32_t indexOffset = 0, indexCount = 0;
    float baseColor[4] = { 1, 1, 1, 1 };
    int image = -1;       // index into images, -1 = none
    bool alphaMask = false;
};

struct CpuModel {
    std::vector<CpuVertex> verts;
    std::vector<uint32_t> indices;
    std::vector<CpuPrim> prims;
    std::vector<CpuImage> images;
    float bmin[3] = { 0, 0, 0 }, bmax[3] = { 0, 0, 0 };
    bool valid = false;
};

// Worker-thread safe. Vertices are pre-transformed to world space.
CpuModel LoadGlbCpu(const std::wstring& path);
// Same contract; handles .fbx and .obj via ufbx.
CpuModel LoadFbxObjCpu(const std::wstring& path);

struct GpuPrim {
    uint32_t indexOffset = 0, indexCount = 0;
    float baseColor[4] = { 1, 1, 1, 1 };
    ID3D11ShaderResourceView* srv = nullptr; // not owned (points into images)
    bool alphaMask = false;
};

struct GpuModel {
    ID3D11Buffer* vb = nullptr;
    ID3D11Buffer* ib = nullptr;
    std::vector<GpuPrim> prims;
    std::vector<ID3D11ShaderResourceView*> images; // owned
    float bmin[3] = { 0, 0, 0 }, bmax[3] = { 0, 0, 0 };

    bool Create(Gfx& gfx, const CpuModel& cpu); // main thread
    void Release();
};

struct OrbitCamera {
    float yawDeg = 35.0f, pitchDeg = -22.0f;
    float dist = 3.0f;
    float center[3] = { 0, 0, 0 };
    float fovDeg = 40.0f;

    void FrameModel(const float bmin[3], const float bmax[3]); // set center+dist from AABB
};

struct ModelRenderer {
    bool Init(Gfx& gfx);
    void Shutdown();
    // Renders into target (sets/clears RTV+DSV+viewport, restores nothing).
    void Render(Gfx& gfx, const GpuModel& model, OffscreenTarget& target,
                const OrbitCamera& cam, const float clearColor[4]);

private:
    ID3D11VertexShader* vs = nullptr;
    ID3D11PixelShader* ps = nullptr;
    ID3D11InputLayout* layout = nullptr;
    ID3D11Buffer* cb = nullptr;
    ID3D11SamplerState* sampler = nullptr;
    ID3D11RasterizerState* raster = nullptr;
    ID3D11DepthStencilState* depthState = nullptr;
    ID3D11BlendState* blend = nullptr;
    ID3D11ShaderResourceView* whiteTex = nullptr;
};
