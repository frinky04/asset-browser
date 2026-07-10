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
    if (audio.IsLooping(previewIndex)) audio.Stop();
    previewIndex = index;
    texZoom = 0.0f;
    texPanX = texPanY = 0.0f;

    SafeRelease(previewTexSRV);
    previewTexW = previewTexH = 0;
    SafeRelease(previewWaveSRV);
    previewPcm = PcmData{};
    audioSelA = audioSelB = -1.0f;
    audioTuneLoop = false;
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
    if (ImGui::Button("Copy path", ImVec2(120, 0))) CopyPathToClipboard(previewIndex);
    ImGui::SameLine();
    if (ImGui::Button("Explorer", ImVec2(120, 0))) OpenInExplorer(previewIndex);
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
            ImGui::Button("Drag out image", ImVec2(-1, 0));
            if (ImGui::IsItemActive() && pendingDrag.empty() &&
                ImGui::IsMouseDragging(ImGuiMouseButton_Left, 6.0f))
                pendingDrag = DragPathsFor(previewIndex);
            if (ImGui::IsItemHovered())
                ImGui::SetTooltip("Drag this button into another app using the current texture export size");
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
                float oldZoom = zoom;
                zoom = (std::max)(0.05f, (std::min)(zoom * powf(1.15f, ImGui::GetIO().MouseWheel), 32.0f));
                float scale = zoom / oldZoom;
                ImVec2 center((a0.x + a1.x) * 0.5f, (a0.y + a1.y) * 0.5f);
                ImVec2 mouse = ImGui::GetIO().MousePos;
                texPanX = scale * texPanX + (1.0f - scale) * (mouse.x - center.x);
                texPanY = scale * texPanY + (1.0f - scale) * (mouse.y - center.y);
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
            if (ImGui::BeginPopupContextItem("texpreviewctx")) {
                if (ImGui::BeginMenu("Drag textures at")) {
                    auto radio = [&](const char* label, int opt) {
                        if (ImGui::MenuItem(label, nullptr, texDragOpt == opt))
                            texDragOpt = opt;
                    };
                    radio("Original size", 0);
                    radio("Half size", -2);
                    radio("Quarter size", -4);
                    ImGui::Separator();
                    radio("Max 512 px", 512);
                    radio("Max 256 px", 256);
                    radio("Max 128 px", 128);
                    radio("Max 64 px", 64);
                    ImGui::EndMenu();
                }
                ImGui::EndPopup();
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

            float dragA = fracAt(ImGui::GetIO().MouseClickedPos[0].x);
            float dragB = fracAt(ImGui::GetIO().MousePos.x);
            if (ImGui::IsItemActive() && fabsf(dragB - dragA) >= 0.005f) {
                if (audio.IsPlaying(previewIndex)) audio.Stop();
                audioSelA = dragA;
                audioSelB = dragB;
            }
            if (ImGui::IsItemDeactivated()) {
                // A plain click preserves the selection and starts/seeks the source audio.
                if (fabsf(dragB - dragA) < 0.005f) {
                    float seekSec = dragA * lenSec;
                    if (audio.IsPlaying(previewIndex) && !audio.IsLooping(previewIndex)) {
                        audio.SeekSeconds(seekSec);
                    } else {
                        if (audio.IsPlaying(previewIndex)) audio.Stop();
                        audio.PlayRange(previewIndex, e.path, seekSec, -1.0f);
                    }
                }
            }
            if (ImGui::IsItemHovered() && ImGui::IsMouseClicked(ImGuiMouseButton_Right)) {
                if (audio.IsPlaying(previewIndex)) audio.Stop();
                audioSelA = audioSelB = -1.0f;
            }

            // selection overlay
            float sa = (std::min)(audioSelA, audioSelB), sb = (std::max)(audioSelA, audioSelB);
            uint64_t loopF0 = 0, loopF1 = 0;
            bool hasSel = AudioSelectionFrames(loopF0, loopF1);
            uint64_t loopRegionFrames = loopF1 - loopF0;
            uint64_t loopFadeFrames = AudioLoopCrossfadeFrames(loopF0, loopF1);
            bool canTuneLoop = loopRegionFrames >= 4 && loopFadeFrames >= 2;
            float tunedLenSec = canTuneLoop
                ? (float)(loopRegionFrames - loopFadeFrames) / previewPcm.sampleRate : 0.0f;
            if (hasSel) {
                float x0 = a0.x + sa * (a1.x - a0.x);
                float x1 = a0.x + sb * (a1.x - a0.x);
                dl->AddRectFilled(ImVec2(x0, a0.y), ImVec2(x1, a1.y), IM_COL32(240, 166, 58, 46));
                dl->AddLine(ImVec2(x0, a0.y), ImVec2(x0, a1.y), Theme::kAccent, 1.0f);
                dl->AddLine(ImVec2(x1, a0.y), ImVec2(x1, a1.y), Theme::kAccent, 1.0f);
            }
            // playback cursor
            if (audio.IsPlaying(previewIndex) && lenSec > 0.0f) {
                float cursorSec = audio.CursorSeconds();
                if (audio.IsLooping(previewIndex))
                    cursorSec += (float)(loopF0 + loopFadeFrames) / previewPcm.sampleRate;
                float cx = a0.x + (cursorSec / lenSec) * (a1.x - a0.x);
                dl->AddLine(ImVec2(cx, a0.y), ImVec2(cx, a1.y), IM_COL32(233, 233, 236, 200), 1.0f);
            }
            if (ImGui::IsItemHovered())
                ImGui::SetTooltip("drag: select region  |  click: play/seek  |  right-click: clear");

            // --- controls ---
            bool playing = audio.IsPlaying(previewIndex);
            const char* playLabel = playing ? "Stop" : audioTuneLoop ? "Play loop" :
                                    hasSel ? "Play selection" : "Play";
            if (audioTuneLoop && !canTuneLoop) ImGui::BeginDisabled();
            if (ImGui::Button(playLabel, ImVec2(120, 0))) TogglePreviewAudio();
            if (audioTuneLoop && !canTuneLoop) ImGui::EndDisabled();
            ImGui::SameLine();
            float contextLenSec = audioTuneLoop ? tunedLenSec :
                                  hasSel ? (sb - sa) * lenSec : lenSec;
            float contextCursorSec = audio.CursorSeconds();
            if (playing && hasSel && !audio.IsLooping(previewIndex))
                contextCursorSec -= sa * lenSec;
            if (playing)
                ImGui::Text("%.2f / %.2f s", contextCursorSec, contextLenSec);
            else
                ImGui::TextDisabled("%.2f s", contextLenSec);

            // --- seamless loop tuning ---
            float tuneLoopWidth = ImGui::GetFrameHeight() + ImGui::GetStyle().ItemInnerSpacing.x +
                                  ImGui::CalcTextSize("Tune loop").x;
            ImGui::SameLine(ImGui::GetWindowWidth() - ImGui::GetStyle().WindowPadding.x -
                            tuneLoopWidth);
            if (ImGui::Checkbox("Tune loop", &audioTuneLoop)) {
                if (audio.IsPlaying(previewIndex)) audio.Stop();
            }
            if (audioTuneLoop) {
                float maxCrossfadeMs = (std::min)(2000.0f,
                    (float)((double)(loopRegionFrames / 2) * 1000.0 / previewPcm.sampleRate));
                if (canTuneLoop) {
                    float minCrossfadeMs = (std::min)(1.0f, maxCrossfadeMs);
                    float displayedCrossfadeMs = (std::max)(minCrossfadeMs,
                        (std::min)(audioLoopCrossfadeMs, maxCrossfadeMs));
                    ImGui::SetNextItemWidth(-1);
                    if (ImGui::SliderFloat("##loopfade", &displayedCrossfadeMs,
                                           minCrossfadeMs, maxCrossfadeMs, "Crossfade %.1f ms",
                                           ImGuiSliderFlags_Logarithmic))
                        audioLoopCrossfadeMs = displayedCrossfadeMs;
                    if (ImGui::IsItemDeactivatedAfterEdit() && audio.IsLooping(previewIndex)) {
                        PcmData loop = BuildTunedAudioLoop();
                        if (loop.valid) audio.PlayLoop(previewIndex, std::move(loop));
                    }
                    ImGui::TextDisabled("Tuned length: %.2f s", tunedLenSec);
                } else {
                    ImGui::TextDisabled("Selection is too short to tune");
                }
            }

            // --- drag-out export ---
            char dragLbl[64];
            if (audioTuneLoop && hasSel)
                snprintf(dragLbl, sizeof(dragLbl), "Drag out tuned loop (%.2fs, wav)",
                         tunedLenSec);
            else if (audioTuneLoop)
                snprintf(dragLbl, sizeof(dragLbl), "Drag out tuned loop (wav)");
            else if (hasSel)
                snprintf(dragLbl, sizeof(dragLbl), "Drag out selection (%.2fs, wav)", (sb - sa) * lenSec);
            else
                snprintf(dragLbl, sizeof(dragLbl), "Drag out full clip (wav)");
            if (audioTuneLoop && !canTuneLoop) ImGui::BeginDisabled();
            ImGui::Button(dragLbl, ImVec2(-1, 0));
            if (ImGui::IsItemActive() && pendingDrag.empty() &&
                ImGui::IsMouseDragging(ImGuiMouseButton_Left, 6.0f)) {
                std::wstring exported = ExportAudioSelection();
                if (!exported.empty()) pendingDrag.push_back(std::move(exported));
            }
            if (ImGui::IsItemHovered())
                ImGui::SetTooltip("Drag this button into another app to export%s as 16-bit WAV",
                                  audioTuneLoop ? " the tuned loop" :
                                  hasSel ? " the selected region" : " the whole clip");
            if (audioTuneLoop && !canTuneLoop) ImGui::EndDisabled();
        } else {
            ImGui::TextDisabled("Failed to decode audio");
        }
    }
}
