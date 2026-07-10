#include "model.h"
#include "util.h"
#include <d3dcompiler.h>
#include <DirectXMath.h>
#include <cmath>
#include <cfloat>
#include <unordered_map>

#include "cgltf.h"
#include "stb_image.h"

using namespace DirectX;

// ---------------------------------------------------------------- CPU loading

namespace {

void TransformPoint(const float m[16], const float in[3], float out[3]) {
    // glTF matrices are column-major
    out[0] = m[0] * in[0] + m[4] * in[1] + m[8] * in[2] + m[12];
    out[1] = m[1] * in[0] + m[5] * in[1] + m[9] * in[2] + m[13];
    out[2] = m[2] * in[0] + m[6] * in[1] + m[10] * in[2] + m[14];
}

void TransformDir(const float m[16], const float in[3], float out[3]) {
    out[0] = m[0] * in[0] + m[4] * in[1] + m[8] * in[2];
    out[1] = m[1] * in[0] + m[5] * in[1] + m[9] * in[2];
    out[2] = m[2] * in[0] + m[6] * in[1] + m[10] * in[2];
}

int LoadEmbeddedImage(CpuModel& out, std::unordered_map<const cgltf_image*, int>& cache,
                      const cgltf_image* img) {
    if (!img) return -1;
    auto it = cache.find(img);
    if (it != cache.end()) return it->second;
    int result = -1;
    if (img->buffer_view && img->buffer_view->buffer && img->buffer_view->buffer->data) {
        const uint8_t* bytes = (const uint8_t*)img->buffer_view->buffer->data + img->buffer_view->offset;
        int w = 0, h = 0, comp = 0;
        stbi_uc* pixels = stbi_load_from_memory(bytes, (int)img->buffer_view->size, &w, &h, &comp, 4);
        if (pixels) {
            CpuImage ci;
            ci.w = w; ci.h = h;
            ci.rgba.assign(pixels, pixels + (size_t)w * h * 4);
            stbi_image_free(pixels);
            out.images.push_back(std::move(ci));
            result = (int)out.images.size() - 1;
        }
    }
    cache.emplace(img, result);
    return result;
}

} // namespace

CpuModel LoadGlbCpu(const std::wstring& path) {
    CpuModel out;
    std::vector<uint8_t> bytes;
    if (!ReadFileBytes(path, bytes) || bytes.empty()) return out;

    cgltf_options opts{};
    cgltf_data* data = nullptr;
    if (cgltf_parse(&opts, bytes.data(), bytes.size(), &data) != cgltf_result_success)
        return out;
    if (cgltf_load_buffers(&opts, data, nullptr) != cgltf_result_success) {
        cgltf_free(data);
        return out;
    }

    std::unordered_map<const cgltf_image*, int> imageCache;
    out.bmin[0] = out.bmin[1] = out.bmin[2] = FLT_MAX;
    out.bmax[0] = out.bmax[1] = out.bmax[2] = -FLT_MAX;

    for (size_t ni = 0; ni < data->nodes_count; ++ni) {
        const cgltf_node* node = &data->nodes[ni];
        if (!node->mesh) continue;
        float world[16];
        cgltf_node_transform_world(node, world);

        for (size_t pi = 0; pi < node->mesh->primitives_count; ++pi) {
            const cgltf_primitive* prim = &node->mesh->primitives[pi];
            if (prim->type != cgltf_primitive_type_triangles) continue;

            const cgltf_accessor* aPos = nullptr;
            const cgltf_accessor* aNrm = nullptr;
            const cgltf_accessor* aUv = nullptr;
            const cgltf_accessor* aCol = nullptr;
            for (size_t ai = 0; ai < prim->attributes_count; ++ai) {
                const cgltf_attribute& at = prim->attributes[ai];
                if (at.type == cgltf_attribute_type_position && at.index == 0) aPos = at.data;
                else if (at.type == cgltf_attribute_type_normal && at.index == 0) aNrm = at.data;
                else if (at.type == cgltf_attribute_type_texcoord && at.index == 0) aUv = at.data;
                else if (at.type == cgltf_attribute_type_color && at.index == 0) aCol = at.data;
            }
            if (!aPos || aPos->count == 0) continue;

            size_t vcount = aPos->count;
            std::vector<float> pos(vcount * 3, 0.0f);
            if (!cgltf_accessor_unpack_floats(aPos, pos.data(), vcount * 3)) continue;

            std::vector<float> nrm;
            if (aNrm && aNrm->count == vcount) {
                nrm.resize(vcount * 3);
                if (!cgltf_accessor_unpack_floats(aNrm, nrm.data(), vcount * 3)) nrm.clear();
            }
            std::vector<float> uv;
            if (aUv && aUv->count == vcount) {
                uv.resize(vcount * 2);
                if (!cgltf_accessor_unpack_floats(aUv, uv.data(), vcount * 2)) uv.clear();
            }
            std::vector<float> col;
            int colComp = 0;
            if (aCol && aCol->count == vcount) {
                colComp = (aCol->type == cgltf_type_vec3) ? 3 : 4;
                col.resize(vcount * colComp);
                if (!cgltf_accessor_unpack_floats(aCol, col.data(), vcount * colComp)) { col.clear(); colComp = 0; }
            }

            uint32_t baseVertex = (uint32_t)out.verts.size();
            uint32_t indexOffset = (uint32_t)out.indices.size();

            for (size_t v = 0; v < vcount; ++v) {
                CpuVertex cv{};
                float wp[3];
                TransformPoint(world, &pos[v * 3], wp);
                cv.px = wp[0]; cv.py = wp[1]; cv.pz = wp[2];
                if (!nrm.empty()) {
                    float wn[3];
                    TransformDir(world, &nrm[v * 3], wn);
                    float len = sqrtf(wn[0] * wn[0] + wn[1] * wn[1] + wn[2] * wn[2]);
                    if (len > 1e-6f) { wn[0] /= len; wn[1] /= len; wn[2] /= len; }
                    cv.nx = wn[0]; cv.ny = wn[1]; cv.nz = wn[2];
                }
                if (!uv.empty()) { cv.u = uv[v * 2]; cv.v = uv[v * 2 + 1]; }
                if (colComp >= 3) {
                    cv.r = col[v * colComp]; cv.g = col[v * colComp + 1]; cv.b = col[v * colComp + 2];
                    cv.a = (colComp == 4) ? col[v * colComp + 3] : 1.0f;
                } else {
                    cv.r = cv.g = cv.b = cv.a = 1.0f;
                }
                out.bmin[0] = (std::min)(out.bmin[0], cv.px); out.bmax[0] = (std::max)(out.bmax[0], cv.px);
                out.bmin[1] = (std::min)(out.bmin[1], cv.py); out.bmax[1] = (std::max)(out.bmax[1], cv.py);
                out.bmin[2] = (std::min)(out.bmin[2], cv.pz); out.bmax[2] = (std::max)(out.bmax[2], cv.pz);
                out.verts.push_back(cv);
            }

            if (prim->indices) {
                for (size_t i = 0; i < prim->indices->count; ++i)
                    out.indices.push_back(baseVertex + (uint32_t)cgltf_accessor_read_index(prim->indices, i));
            } else {
                for (size_t i = 0; i < vcount; ++i)
                    out.indices.push_back(baseVertex + (uint32_t)i);
            }

            // Flat normals if the mesh had none
            if (nrm.empty()) {
                for (size_t i = indexOffset; i + 2 < out.indices.size(); i += 3) {
                    CpuVertex& v0 = out.verts[out.indices[i]];
                    CpuVertex& v1 = out.verts[out.indices[i + 1]];
                    CpuVertex& v2 = out.verts[out.indices[i + 2]];
                    float e1[3] = { v1.px - v0.px, v1.py - v0.py, v1.pz - v0.pz };
                    float e2[3] = { v2.px - v0.px, v2.py - v0.py, v2.pz - v0.pz };
                    float n[3] = { e1[1] * e2[2] - e1[2] * e2[1],
                                   e1[2] * e2[0] - e1[0] * e2[2],
                                   e1[0] * e2[1] - e1[1] * e2[0] };
                    for (CpuVertex* vv : { &v0, &v1, &v2 }) {
                        vv->nx += n[0]; vv->ny += n[1]; vv->nz += n[2];
                    }
                }
                for (size_t v = baseVertex; v < out.verts.size(); ++v) {
                    CpuVertex& cv = out.verts[v];
                    float len = sqrtf(cv.nx * cv.nx + cv.ny * cv.ny + cv.nz * cv.nz);
                    if (len > 1e-6f) { cv.nx /= len; cv.ny /= len; cv.nz /= len; }
                    else { cv.ny = 1.0f; }
                }
            }

            CpuPrim cp;
            cp.indexOffset = indexOffset;
            cp.indexCount = (uint32_t)(out.indices.size() - indexOffset);
            if (prim->material) {
                const cgltf_material* mat = prim->material;
                if (mat->has_pbr_metallic_roughness) {
                    memcpy(cp.baseColor, mat->pbr_metallic_roughness.base_color_factor, sizeof(cp.baseColor));
                    if (mat->pbr_metallic_roughness.base_color_texture.texture)
                        cp.image = LoadEmbeddedImage(out, imageCache,
                                                     mat->pbr_metallic_roughness.base_color_texture.texture->image);
                }
                cp.alphaMask = (mat->alpha_mode == cgltf_alpha_mode_mask);
            }
            out.prims.push_back(cp);
        }
    }

    cgltf_free(data);
    if (out.verts.empty() || out.indices.empty()) return out;
    out.valid = true;
    return out;
}

// ---------------------------------------------------------------- GPU

bool GpuModel::Create(Gfx& gfx, const CpuModel& cpu) {
    if (!cpu.valid) return false;
    D3D11_BUFFER_DESC bd{};
    bd.Usage = D3D11_USAGE_IMMUTABLE;
    bd.ByteWidth = (UINT)(cpu.verts.size() * sizeof(CpuVertex));
    bd.BindFlags = D3D11_BIND_VERTEX_BUFFER;
    D3D11_SUBRESOURCE_DATA init{ cpu.verts.data(), 0, 0 };
    if (FAILED(gfx.device->CreateBuffer(&bd, &init, &vb))) return false;

    bd.ByteWidth = (UINT)(cpu.indices.size() * sizeof(uint32_t));
    bd.BindFlags = D3D11_BIND_INDEX_BUFFER;
    init.pSysMem = cpu.indices.data();
    if (FAILED(gfx.device->CreateBuffer(&bd, &init, &ib))) { Release(); return false; }

    images.reserve(cpu.images.size());
    for (const CpuImage& ci : cpu.images)
        images.push_back(gfx.CreateTextureSRV(ci.rgba.data(), ci.w, ci.h, /*srgb*/ true));

    prims.reserve(cpu.prims.size());
    for (const CpuPrim& cp : cpu.prims) {
        GpuPrim gp;
        gp.indexOffset = cp.indexOffset;
        gp.indexCount = cp.indexCount;
        memcpy(gp.baseColor, cp.baseColor, sizeof(gp.baseColor));
        gp.srv = (cp.image >= 0 && cp.image < (int)images.size()) ? images[cp.image] : nullptr;
        gp.alphaMask = cp.alphaMask;
        prims.push_back(gp);
    }
    memcpy(bmin, cpu.bmin, sizeof(bmin));
    memcpy(bmax, cpu.bmax, sizeof(bmax));
    return true;
}

void GpuModel::Release() {
    for (auto* s : images) SafeRelease(s);
    images.clear();
    prims.clear();
    SafeRelease(vb);
    SafeRelease(ib);
}

void OrbitCamera::FrameModel(const float mn[3], const float mx[3]) {
    center[0] = (mn[0] + mx[0]) * 0.5f;
    center[1] = (mn[1] + mx[1]) * 0.5f;
    center[2] = (mn[2] + mx[2]) * 0.5f;
    float dx = mx[0] - mn[0], dy = mx[1] - mn[1], dz = mx[2] - mn[2];
    float radius = 0.5f * sqrtf(dx * dx + dy * dy + dz * dz);
    if (radius < 1e-4f) radius = 1.0f;
    dist = radius / tanf(XMConvertToRadians(fovDeg) * 0.5f) * 1.15f;
}

// ---------------------------------------------------------------- Renderer

namespace {

const char* kShaderSrc = R"hlsl(
cbuffer CB : register(b0) {
    float4x4 viewProj;
    float4 baseColor;
    float4 keyLightDir;   // xyz
    float4 fillLightDir;  // xyz
    float4 misc;          // x=ambient, y=alphaMask, z=hasTexture
};
Texture2D tex0 : register(t0);
SamplerState smp0 : register(s0);

struct VSIn  { float3 pos : POSITION; float3 nrm : NORMAL; float2 uv : TEXCOORD0; float4 col : COLOR0; };
struct VSOut { float4 pos : SV_Position; float3 nrm : NORMAL; float2 uv : TEXCOORD0; float4 col : COLOR0; };

VSOut vsmain(VSIn i) {
    VSOut o;
    o.pos = mul(float4(i.pos, 1.0), viewProj);
    o.nrm = i.nrm;
    o.uv = i.uv;
    o.col = i.col;
    return o;
}

float4 psmain(VSOut i) : SV_Target {
    float4 t = (misc.z > 0.5) ? tex0.Sample(smp0, i.uv) : float4(1, 1, 1, 1);
    float4 albedo = baseColor * i.col * t;
    if (misc.y > 0.5 && albedo.a < 0.5) discard;
    float3 n = normalize(i.nrm);
    float ndk = saturate(dot(n, -keyLightDir.xyz));
    float ndf = saturate(dot(n, -fillLightDir.xyz));
    float3 c = albedo.rgb * (misc.x + ndk * 0.95 + ndf * 0.30);
    return float4(c, 1.0);
}
)hlsl";

struct CBData {
    XMFLOAT4X4 viewProj;
    float baseColor[4];
    float keyLightDir[4];
    float fillLightDir[4];
    float misc[4];
};

} // namespace

bool ModelRenderer::Init(Gfx& gfx) {
    ID3DBlob* vsb = nullptr, * psb = nullptr, * err = nullptr;
    if (FAILED(D3DCompile(kShaderSrc, strlen(kShaderSrc), nullptr, nullptr, nullptr,
                          "vsmain", "vs_5_0", 0, 0, &vsb, &err))) {
        if (err) { OutputDebugStringA((const char*)err->GetBufferPointer()); err->Release(); }
        return false;
    }
    if (FAILED(D3DCompile(kShaderSrc, strlen(kShaderSrc), nullptr, nullptr, nullptr,
                          "psmain", "ps_5_0", 0, 0, &psb, &err))) {
        if (err) { OutputDebugStringA((const char*)err->GetBufferPointer()); err->Release(); }
        vsb->Release();
        return false;
    }
    gfx.device->CreateVertexShader(vsb->GetBufferPointer(), vsb->GetBufferSize(), nullptr, &vs);
    gfx.device->CreatePixelShader(psb->GetBufferPointer(), psb->GetBufferSize(), nullptr, &ps);

    D3D11_INPUT_ELEMENT_DESC il[] = {
        { "POSITION", 0, DXGI_FORMAT_R32G32B32_FLOAT,    0, 0,  D3D11_INPUT_PER_VERTEX_DATA, 0 },
        { "NORMAL",   0, DXGI_FORMAT_R32G32B32_FLOAT,    0, 12, D3D11_INPUT_PER_VERTEX_DATA, 0 },
        { "TEXCOORD", 0, DXGI_FORMAT_R32G32_FLOAT,       0, 24, D3D11_INPUT_PER_VERTEX_DATA, 0 },
        { "COLOR",    0, DXGI_FORMAT_R32G32B32A32_FLOAT, 0, 32, D3D11_INPUT_PER_VERTEX_DATA, 0 },
    };
    gfx.device->CreateInputLayout(il, 4, vsb->GetBufferPointer(), vsb->GetBufferSize(), &layout);
    vsb->Release(); psb->Release();

    D3D11_BUFFER_DESC cbd{};
    cbd.ByteWidth = sizeof(CBData);
    cbd.Usage = D3D11_USAGE_DYNAMIC;
    cbd.BindFlags = D3D11_BIND_CONSTANT_BUFFER;
    cbd.CPUAccessFlags = D3D11_CPU_ACCESS_WRITE;
    gfx.device->CreateBuffer(&cbd, nullptr, &cb);

    D3D11_SAMPLER_DESC smp{};
    smp.Filter = D3D11_FILTER_MIN_MAG_MIP_LINEAR;
    smp.AddressU = smp.AddressV = smp.AddressW = D3D11_TEXTURE_ADDRESS_WRAP;
    smp.MaxLOD = D3D11_FLOAT32_MAX;
    gfx.device->CreateSamplerState(&smp, &sampler);

    D3D11_RASTERIZER_DESC rs{};
    rs.FillMode = D3D11_FILL_SOLID;
    rs.CullMode = D3D11_CULL_NONE; // PSX-era assets have inconsistent winding
    rs.DepthClipEnable = TRUE;
    gfx.device->CreateRasterizerState(&rs, &raster);

    D3D11_DEPTH_STENCIL_DESC ds{};
    ds.DepthEnable = TRUE;
    ds.DepthWriteMask = D3D11_DEPTH_WRITE_MASK_ALL;
    ds.DepthFunc = D3D11_COMPARISON_LESS;
    gfx.device->CreateDepthStencilState(&ds, &depthState);

    D3D11_BLEND_DESC bl{};
    bl.RenderTarget[0].RenderTargetWriteMask = D3D11_COLOR_WRITE_ENABLE_ALL;
    gfx.device->CreateBlendState(&bl, &blend);

    uint8_t white[4] = { 255, 255, 255, 255 };
    whiteTex = gfx.CreateTextureSRV(white, 1, 1, true);
    return vs && ps && layout && cb;
}

void ModelRenderer::Shutdown() {
    SafeRelease(whiteTex);
    SafeRelease(blend);
    SafeRelease(depthState);
    SafeRelease(raster);
    SafeRelease(sampler);
    SafeRelease(cb);
    SafeRelease(layout);
    SafeRelease(ps);
    SafeRelease(vs);
}

void ModelRenderer::Render(Gfx& gfx, const GpuModel& model, OffscreenTarget& target,
                           const OrbitCamera& cam, const float clearColor[4]) {
    ID3D11DeviceContext* c = gfx.ctx;

    c->OMSetRenderTargets(1, &target.rtv, target.dsv);
    c->ClearRenderTargetView(target.rtv, clearColor);
    c->ClearDepthStencilView(target.dsv, D3D11_CLEAR_DEPTH | D3D11_CLEAR_STENCIL, 1.0f, 0);
    D3D11_VIEWPORT vp{ 0, 0, (float)target.w, (float)target.h, 0, 1 };
    c->RSSetViewports(1, &vp);

    float yaw = XMConvertToRadians(cam.yawDeg);
    float pitch = XMConvertToRadians(cam.pitchDeg);
    XMVECTOR centerV = XMVectorSet(cam.center[0], cam.center[1], cam.center[2], 1);
    XMVECTOR offset = XMVectorSet(cosf(pitch) * sinf(yaw), -sinf(pitch), cosf(pitch) * cosf(yaw), 0);
    XMVECTOR eye = XMVectorAdd(centerV, XMVectorScale(offset, cam.dist));
    XMMATRIX view = XMMatrixLookAtRH(eye, centerV, XMVectorSet(0, 1, 0, 0));
    float aspect = (float)target.w / (float)target.h;
    XMMATRIX proj = XMMatrixPerspectiveFovRH(XMConvertToRadians(cam.fovDeg), aspect,
                                             cam.dist * 0.01f, cam.dist * 10.0f + 100.0f);
    XMMATRIX viewProj = XMMatrixTranspose(XMMatrixMultiply(view, proj));

    UINT stride = sizeof(CpuVertex), offsetB = 0;
    c->IASetInputLayout(layout);
    c->IASetVertexBuffers(0, 1, &model.vb, &stride, &offsetB);
    c->IASetIndexBuffer(model.ib, DXGI_FORMAT_R32_UINT, 0);
    c->IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
    c->VSSetShader(vs, nullptr, 0);
    c->PSSetShader(ps, nullptr, 0);
    c->VSSetConstantBuffers(0, 1, &cb);
    c->PSSetConstantBuffers(0, 1, &cb);
    c->PSSetSamplers(0, 1, &sampler);
    c->RSSetState(raster);
    c->OMSetDepthStencilState(depthState, 0);
    float bf[4] = { 0, 0, 0, 0 };
    c->OMSetBlendState(blend, bf, 0xFFFFFFFF);

    for (const GpuPrim& p : model.prims) {
        CBData d{};
        XMStoreFloat4x4(&d.viewProj, viewProj);
        memcpy(d.baseColor, p.baseColor, sizeof(d.baseColor));
        // Key light from upper-front-left (camera-ish space), fill from opposite side
        XMVECTOR key = XMVector3Normalize(XMVectorSet(-0.5f, -0.8f, -0.6f, 0));
        XMVECTOR fill = XMVector3Normalize(XMVectorSet(0.7f, -0.1f, 0.5f, 0));
        XMStoreFloat4((XMFLOAT4*)d.keyLightDir, key);
        XMStoreFloat4((XMFLOAT4*)d.fillLightDir, fill);
        d.misc[0] = 0.30f;                       // ambient
        d.misc[1] = p.alphaMask ? 1.0f : 0.0f;
        d.misc[2] = p.srv ? 1.0f : 0.0f;

        D3D11_MAPPED_SUBRESOURCE map{};
        if (SUCCEEDED(c->Map(cb, 0, D3D11_MAP_WRITE_DISCARD, 0, &map))) {
            memcpy(map.pData, &d, sizeof(d));
            c->Unmap(cb, 0);
        }
        ID3D11ShaderResourceView* srv = p.srv ? p.srv : whiteTex;
        c->PSSetShaderResources(0, 1, &srv);
        c->DrawIndexed(p.indexCount, p.indexOffset, 0);
    }

    // Unbind so the target's SRV can be sampled by ImGui afterwards
    ID3D11RenderTargetView* nullRTV = nullptr;
    c->OMSetRenderTargets(1, &nullRTV, nullptr);
}
