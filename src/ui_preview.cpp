#include "app.h"
#include "util.h"
#include "theme.h"
#include "imgui.h"
#include <filesystem>
#include <algorithm>

#include "stb_image.h"
#include "stb_image_resize2.h"

namespace fs = std::filesystem;

void App::SetPreview(int index) {
    if (index == previewIndex) return;
    previewIndex = index;
    texZoom = 0.0f;
    texPanX = texPanY = 0.0f;

    SafeRelease(previewTexSRV);
    previewTexW = previewTexH = 0;
    SafeRelease(previewWaveSRV);
    previewPcm = PcmData{};
    audioSelA = audioSelB = -1.0f;
    previewModel.Release();
    previewModelValid = false;
    if (index < 0 || index >= (int)db.entries.size()) return;
    const AssetEntry& e = db.entries[index];

    if (e.type == AssetType::Audio) {
        previewPcm = DecodeAudioFile(e.path);
        if (previewPcm.valid) {
            std::vector<uint8_t> px = RenderWaveformThumb(previewPcm, 1024, 256);
            previewWaveSRV = gfx->CreateTextureSRV(px.data(), 1024, 256, false);
        }
        return;
    }

    if (e.type == AssetType::Texture) {
        BuildTexturePreview();
        return;
    }
    if (e.type == AssetType::Model && (e.ext == ".glb" || e.ext == ".gltf")) {
        CpuModel cpu = LoadGlbCpu(e.path);
        if (cpu.valid && previewModel.Create(*gfx, cpu)) {
            previewModelValid = true;
            previewCam = OrbitCamera{};
            previewCam.FrameModel(previewModel.bmin, previewModel.bmax);
        }
    }
}

// Builds the preview image for the selected texture. When a drag-size mode is
// active the preview is built at export resolution, so what you see is exactly
// what a drag will hand over.
void App::BuildTexturePreview() {
    SafeRelease(previewTexSRV);
    previewTexW = previewTexH = previewTexSrcW = previewTexSrcH = 0;
    previewTexBuiltOpt = texDragOpt;
    if (previewIndex < 0 || previewIndex >= (int)db.entries.size()) return;
    const AssetEntry& e = db.entries[previewIndex];
    if (e.type != AssetType::Texture) return;

    std::vector<uint8_t> bytes;
    if (!ReadFileBytes(e.path, bytes) || bytes.empty()) return;
    int w = 0, h = 0, comp = 0;
    stbi_uc* px = stbi_load_from_memory(bytes.data(), (int)bytes.size(), &w, &h, &comp, 4);
    if (!px) return;
    previewTexSrcW = w; previewTexSrcH = h;

    int nw = 0, nh = 0;
    bool downres = DragDims(w, h, nw, nh);
    if (!downres) {
        // display cap only (huge sources) — export would be the original file
        const int cap = 2048;
        nw = w; nh = h;
        if (w > cap || h > cap) {
            float s = (std::min)((float)cap / w, (float)cap / h);
            nw = (std::max)(1, (int)(w * s));
            nh = (std::max)(1, (int)(h * s));
        }
    }
    if (nw == w && nh == h) {
        previewTexSRV = gfx->CreateTextureSRV(px, w, h, false);
    } else {
        std::vector<uint8_t> resized((size_t)nw * nh * 4);
        stbir_resize_uint8_srgb(px, w, h, w * 4, resized.data(), nw, nh, nw * 4, STBIR_RGBA);
        previewTexSRV = gfx->CreateTextureSRV(resized.data(), nw, nh, false);
    }
    previewTexW = nw; previewTexH = nh;
    stbi_image_free(px);
}

void App::DrawPreview() {
    if (previewIndex < 0 || previewIndex >= (int)db.entries.size()) {
        ImGui::TextDisabled("Select an asset to preview");
        return;
    }
    const AssetEntry& e = db.entries[previewIndex];

    ImGui::TextWrapped("%s", e.name.c_str());
    ImGui::TextDisabled("%s", e.relPath.c_str());
    if (e.size >= 1024 * 1024)
        ImGui::TextDisabled("%.2f MB", e.size / (1024.0 * 1024.0));
    else
        ImGui::TextDisabled("%.1f KB", e.size / 1024.0);
    if (ImGui::SmallButton("Copy path")) CopyPathToClipboard(previewIndex);
    ImGui::SameLine();
    if (ImGui::SmallButton("Explorer")) OpenInExplorer(previewIndex);
    ImGui::Separator();

    ImVec2 avail = ImGui::GetContentRegionAvail();

    if (e.type == AssetType::Texture) {
        if (previewTexBuiltOpt != texDragOpt)
            BuildTexturePreview(); // drag-size mode changed since the preview was built
        const Thumb& th = thumbs.Get(previewIndex);
        ID3D11ShaderResourceView* srv = previewTexSRV ? previewTexSRV : th.srv;
        int tw = previewTexSRV ? previewTexW : th.w;
        int thh = previewTexSRV ? previewTexH : th.h;
        struct { ID3D11ShaderResourceView* srv; int w, h; } t{ srv, tw, thh };
        if (t.srv) {
            if (ImGui::Checkbox("Tile", &texTile)) { /* wrap-mode preview */ }
            ImGui::SameLine();
            ImGui::TextDisabled("wheel: zoom, drag: pan, dbl-click: reset");
            if (previewTexSRV && (previewTexW < previewTexSrcW || previewTexH < previewTexSrcH) &&
                texDragOpt != 0)
                ImGui::TextColored(Theme::Accent(0.9f), "Previewing export size %dx%d (source %dx%d)",
                                   previewTexW, previewTexH, previewTexSrcW, previewTexSrcH);
            avail = ImGui::GetContentRegionAvail();
            float areaH = avail.y - 4;
            ImVec2 areaSize(avail.x, areaH);
            ImGui::InvisibleButton("texarea", areaSize);
            ImVec2 a0 = ImGui::GetItemRectMin();
            ImVec2 a1 = ImGui::GetItemRectMax();
            ImDrawList* dl = ImGui::GetWindowDrawList();
            dl->PushClipRect(a0, a1, true);

            // checkerboard
            const float cs = 12.0f;
            for (int y = 0; y < (int)(areaSize.y / cs) + 1; ++y)
                for (int x = 0; x < (int)(areaSize.x / cs) + 1; ++x) {
                    if ((x + y) & 1) continue;
                    ImVec2 q0(a0.x + x * cs, a0.y + y * cs);
                    ImVec2 q1((std::min)(q0.x + cs, a1.x), (std::min)(q0.y + cs, a1.y));
                    dl->AddRectFilled(q0, q1, IM_COL32(60, 60, 66, 255));
                }

            float fit = (std::min)(areaSize.x / t.w, areaSize.y / t.h);
            fit = (std::min)(fit, 4.0f);
            float zoom = (texZoom <= 0.0f) ? fit : texZoom;
            if (ImGui::IsItemHovered() && ImGui::GetIO().MouseWheel != 0) {
                zoom *= powf(1.15f, ImGui::GetIO().MouseWheel);
                zoom = (std::max)(0.05f, (std::min)(zoom, 32.0f));
                texZoom = zoom;
            }
            if (ImGui::IsItemActive() && ImGui::IsMouseDragging(ImGuiMouseButton_Left, 0)) {
                texPanX += ImGui::GetIO().MouseDelta.x;
                texPanY += ImGui::GetIO().MouseDelta.y;
            }
            float w = t.w * zoom, h = t.h * zoom;
            ImVec2 cpos((a0.x + a1.x - w) * 0.5f + texPanX, (a0.y + a1.y - h) * 0.5f + texPanY);
            if (texTile) {
                // Fill the whole area with out-of-range UVs. The ImGui backend
                // sampler clamps, so swap in a wrapping sampler for this draw.
                ImVec2 uv0((a0.x - cpos.x) / w, (a0.y - cpos.y) / h);
                ImVec2 uv1((a1.x - cpos.x) / w, (a1.y - cpos.y) / h);
                dl->AddCallback(
                    [](const ImDrawList*, const ImDrawCmd* cmd) {
                        Gfx* g = (Gfx*)cmd->UserCallbackData;
                        g->ctx->PSSetSamplers(0, 1, &g->wrapSampler);
                    },
                    gfx);
                dl->AddImage((ImTextureID)(intptr_t)t.srv, a0, a1, uv0, uv1);
                dl->AddCallback(ImDrawCallback_ResetRenderState, nullptr);
                // outline one tile for reference
                dl->AddRect(cpos, ImVec2(cpos.x + w, cpos.y + h), IM_COL32(255, 255, 255, 60));
            } else {
                dl->AddImage((ImTextureID)(intptr_t)t.srv, cpos, ImVec2(cpos.x + w, cpos.y + h));
            }
            dl->PopClipRect();

            if (ImGui::IsItemHovered())
                ImGui::SetTooltip("%dx%d", t.w, t.h);
            if (ImGui::IsItemHovered() && ImGui::IsMouseDoubleClicked(ImGuiMouseButton_Left)) {
                texZoom = 0;
                texPanX = texPanY = 0;
            }
        } else {
            ImGui::TextDisabled("Loading...");
        }
    } else if (e.type == AssetType::Model) {
        if (previewModelValid) {
            int w = (std::max)(64, (int)avail.x);
            int h = (std::max)(64, (int)(avail.y - 4));
            if (previewRT.w != w || previewRT.h != h)
                previewRT.Create(gfx->device, w, h, false);

            float clear[4] = { 0.0106f, 0.0106f, 0.0116f, 1.0f }; // ~#1A1A1D after sRGB encode
            renderer.Render(*gfx, previewModel, previewRT, previewCam, clear);

            ImGui::InvisibleButton("orbit", ImVec2((float)w, (float)h));
            ImVec2 a0 = ImGui::GetItemRectMin();
            ImGui::GetWindowDrawList()->AddImage((ImTextureID)(intptr_t)previewRT.srv,
                                                 a0, ImVec2(a0.x + w, a0.y + h));
            if (ImGui::IsItemActive() && ImGui::IsMouseDragging(ImGuiMouseButton_Left, 0)) {
                previewCam.yawDeg -= ImGui::GetIO().MouseDelta.x * 0.5f;
                previewCam.pitchDeg -= ImGui::GetIO().MouseDelta.y * 0.5f;
                previewCam.pitchDeg = (std::max)(-89.0f, (std::min)(89.0f, previewCam.pitchDeg));
            }
            if (ImGui::IsItemHovered() && ImGui::GetIO().MouseWheel != 0)
                previewCam.dist *= powf(0.88f, ImGui::GetIO().MouseWheel);
        } else {
            ImGui::TextDisabled(e.ext == ".glb" ? "Failed to load model"
                                                : "No GLB available for preview (%s)", e.ext.c_str());
            if (!e.variants.empty()) {
                ImGui::TextDisabled("Formats on disk:");
                ImGui::BulletText("%s", e.ext.c_str());
                for (const std::wstring& v : e.variants) {
                    fs::path vp(v);
                    ImGui::BulletText("%s", ToLowerAscii(WideToUtf8(vp.extension().wstring())).c_str());
                }
            }
        }
    } else { // Audio
        float lenSec = previewPcm.valid && previewPcm.sampleRate
                           ? (float)previewPcm.frames / previewPcm.sampleRate : 0.0f;

        // --- waveform with drag-to-select ---
        if (previewWaveSRV) {
            float wW = avail.x;
            float wH = (std::min)(170.0f, avail.y * 0.45f);
            ImGui::InvisibleButton("wave", ImVec2(wW, wH));
            ImVec2 a0 = ImGui::GetItemRectMin();
            ImVec2 a1 = ImGui::GetItemRectMax();
            ImDrawList* dl = ImGui::GetWindowDrawList();
            dl->AddImage((ImTextureID)(intptr_t)previewWaveSRV, a0, a1);

            auto fracAt = [&](float x) {
                return (std::max)(0.0f, (std::min)(1.0f, (x - a0.x) / (a1.x - a0.x)));
            };

            if (ImGui::IsItemActive()) {
                audioSelA = fracAt(ImGui::GetIO().MouseClickedPos[0].x);
                audioSelB = fracAt(ImGui::GetIO().MousePos.x);
            }
            if (ImGui::IsItemDeactivated()) {
                // a plain click (no drag) clears the selection and seeks
                if (fabsf(audioSelB - audioSelA) < 0.005f) {
                    if (audio.IsPlaying(previewIndex))
                        audio.SeekSeconds(audioSelA * lenSec);
                    audioSelA = audioSelB = -1.0f;
                }
            }
            if (ImGui::IsItemHovered() && ImGui::IsMouseClicked(ImGuiMouseButton_Right))
                audioSelA = audioSelB = -1.0f;

            // selection overlay
            float sa = (std::min)(audioSelA, audioSelB), sb = (std::max)(audioSelA, audioSelB);
            bool hasSel = sa >= 0.0f && (sb - sa) > 0.001f;
            if (hasSel) {
                float x0 = a0.x + sa * (a1.x - a0.x);
                float x1 = a0.x + sb * (a1.x - a0.x);
                dl->AddRectFilled(ImVec2(x0, a0.y), ImVec2(x1, a1.y), IM_COL32(240, 166, 58, 46));
                dl->AddLine(ImVec2(x0, a0.y), ImVec2(x0, a1.y), Theme::kAccent, 1.0f);
                dl->AddLine(ImVec2(x1, a0.y), ImVec2(x1, a1.y), Theme::kAccent, 1.0f);
            }
            // playback cursor
            if (audio.IsPlaying(previewIndex) && lenSec > 0.0f) {
                float cx = a0.x + (audio.CursorSeconds() / lenSec) * (a1.x - a0.x);
                dl->AddLine(ImVec2(cx, a0.y), ImVec2(cx, a1.y), IM_COL32(233, 233, 236, 200), 1.0f);
            }
            if (ImGui::IsItemHovered())
                ImGui::SetTooltip("drag: select region  |  click: seek  |  right-click: clear");

            // --- controls ---
            bool playing = audio.IsPlaying(previewIndex);
            if (ImGui::Button(playing ? "Stop" : "Play", ImVec2(70, 0))) {
                if (playing) audio.Stop();
                else audio.Toggle(previewIndex, e.path);
            }
            ImGui::SameLine();
            if (hasSel) {
                if (ImGui::Button("Play selection"))
                    audio.PlayRange(previewIndex, e.path, sa * lenSec, sb * lenSec);
                ImGui::SameLine();
            }
            if (playing)
                ImGui::Text("%.2f / %.2f s", audio.CursorSeconds(), lenSec);
            else
                ImGui::TextDisabled("%.2f s", lenSec);

            // --- drag-out export ---
            char dragLbl[64];
            if (hasSel)
                snprintf(dragLbl, sizeof(dragLbl), "Drag out selection (%.2fs, wav)", (sb - sa) * lenSec);
            else
                snprintf(dragLbl, sizeof(dragLbl), "Drag out full clip (wav)");
            ImGui::Button(dragLbl, ImVec2(-1, 0));
            if (ImGui::IsItemActive() && pendingDrag.empty() &&
                ImGui::IsMouseDragging(ImGuiMouseButton_Left, 6.0f)) {
                std::wstring exported = ExportAudioSelection();
                if (!exported.empty()) pendingDrag.push_back(std::move(exported));
            }
            if (ImGui::IsItemHovered())
                ImGui::SetTooltip("Drag this button into another app to export%s as 16-bit WAV",
                                  hasSel ? " the selected region" : " the whole clip");
        } else {
            ImGui::TextDisabled("Failed to decode audio");
        }
    }
}
