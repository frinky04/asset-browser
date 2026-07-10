#include "app.h"
#include "util.h"
#include "theme.h"
#include "imgui.h"
#include <filesystem>
#include <algorithm>

namespace fs = std::filesystem;

namespace {

ImU32 TypeColor(AssetType t) {
    switch (t) {
        case AssetType::Texture: return IM_COL32(86, 156, 214, 255);
        case AssetType::Model:   return IM_COL32(181, 206, 168, 255);
        case AssetType::Audio:   return IM_COL32(220, 170, 110, 255);
    }
    return IM_COL32(160, 160, 160, 255);
}

const char* ExtBadge(const AssetEntry& e) {
    if (e.ext.size() > 1) return e.ext.c_str() + 1; // skip the dot
    return "?";
}

} // namespace

void App::DrawGrid() {
    // --- search bar ---
    if (focusSearch) {
        ImGui::SetKeyboardFocusHere();
        focusSearch = false;
    }
    float clearW = ImGui::CalcTextSize("Clear").x + ImGui::GetStyle().FramePadding.x * 2.0f;
    ImGui::SetNextItemWidth(ImGui::GetContentRegionAvail().x - clearW - ImGui::GetStyle().ItemSpacing.x);
    if (ImGui::InputTextWithHint("##search", "Search assets...  (Ctrl+F)", searchBuf, sizeof(searchBuf)))
        filterDirty = true;
    ImGui::SameLine();
    if (ImGui::Button("Clear") && searchBuf[0]) {
        searchBuf[0] = 0;
        filterDirty = true;
    }
    ImGui::Dummy(ImVec2(0, 2.0f));

    // --- virtualized tile grid ---
    ImGui::BeginChild("grid", ImVec2(0, 0), ImGuiChildFlags_None);

    const float pad = 8.0f;
    const float labelH = ImGui::GetTextLineHeight() + 4.0f;
    const float cellH = tileSize + labelH + pad;
    float availW = ImGui::GetContentRegionAvail().x - ImGui::GetStyle().ScrollbarSize;
    // Fit as many ideal-width columns as possible, then stretch cells so the row
    // exactly fills the pane (no dead gap at the right edge).
    int cols = (std::max)(1, (int)(availW / (tileSize + pad)));
    const float cellW = availW / cols;
    int count = (int)filtered.size();
    int rows = (count + cols - 1) / cols;

    ImGui::Dummy(ImVec2(0, rows * cellH)); // establish scroll extent

    float scrollY = ImGui::GetScrollY();
    float viewH = ImGui::GetWindowHeight();
    int firstRow = (std::max)(0, (int)(scrollY / cellH) - 1);
    int lastRow = (std::min)(rows - 1, (int)((scrollY + viewH) / cellH) + 1);

    ImDrawList* dl = ImGui::GetWindowDrawList();
    ImGuiIO& io = ImGui::GetIO();

    for (int r = firstRow; r <= lastRow; ++r) {
        for (int c = 0; c < cols; ++c) {
            int slot = r * cols + c;
            if (slot >= count) break;
            int idx = filtered[slot];
            const AssetEntry& e = db.entries[idx];

            ImGui::PushID(idx);
            // center the fixed-size tile inside the stretched cell
            ImGui::SetCursorPos(ImVec2(c * cellW + (cellW - tileSize) * 0.5f, r * cellH + pad * 0.5f));
            ImGui::InvisibleButton("tile", ImVec2(tileSize, tileSize + labelH));
            bool hovered = ImGui::IsItemHovered();
            bool selected = selection.count(idx) > 0;

            ImVec2 p0 = ImGui::GetItemRectMin();
            ImVec2 p1 = ImGui::GetItemRectMax();
            ImVec2 imgP0 = p0;
            ImVec2 imgP1 = ImVec2(p0.x + tileSize, p0.y + tileSize);

            // background
            dl->AddRectFilled(p0, p1, hovered ? Theme::kTileBgHover : Theme::kTileBg);
            if (selected) {
                dl->AddRectFilled(p0, p1, Theme::kSelectionFill);
                dl->AddRect(p0, p1, Theme::kSelectionBorder, 0.0f, 0, 1.0f);
            }

            // request + draw thumbnail
            thumbs.Request(idx, e);
            const Thumb& t = thumbs.Get(idx);
            if (t.state == ThumbState::Ready && t.srv) {
                // fit into square preserving aspect
                float sw = tileSize - 8, sh = tileSize - 8;
                float s = (std::min)(sw / t.w, sh / t.h);
                float w = t.w * s, h = t.h * s;
                ImVec2 cpos((imgP0.x + imgP1.x - w) * 0.5f, (imgP0.y + imgP1.y - h) * 0.5f);
                dl->AddImage((ImTextureID)(intptr_t)t.srv, cpos, ImVec2(cpos.x + w, cpos.y + h));
            } else {
                // placeholder: rounded box + extension badge
                ImVec2 c0(imgP0.x + tileSize * 0.22f, imgP0.y + tileSize * 0.22f);
                ImVec2 c1(imgP1.x - tileSize * 0.22f, imgP1.y - tileSize * 0.22f);
                ImU32 col = TypeColor(e.type);
                dl->AddRect(c0, c1, col, 6.0f, 0, 2.0f);
                const char* badge = ExtBadge(e);
                ImVec2 ts = ImGui::CalcTextSize(badge);
                dl->AddText(ImVec2((c0.x + c1.x - ts.x) * 0.5f, (c0.y + c1.y - ts.y) * 0.5f),
                            col, badge);
            }

            // audio playing indicator
            if (e.type == AssetType::Audio && audio.IsPlaying(idx)) {
                ImVec2 ic(imgP1.x - 16, imgP0.y + 16);
                dl->AddCircleFilled(ic, 9.0f, IM_COL32(40, 200, 90, 230));
                dl->AddRectFilled(ImVec2(ic.x - 4, ic.y - 4), ImVec2(ic.x - 1.5f, ic.y + 4), IM_COL32_WHITE);
                dl->AddRectFilled(ImVec2(ic.x + 1.5f, ic.y - 4), ImVec2(ic.x + 4, ic.y + 4), IM_COL32_WHITE);
            }

            // model variant count badge
            if (e.type == AssetType::Model && !e.variants.empty()) {
                char buf[16];
                snprintf(buf, sizeof(buf), "+%d", (int)e.variants.size());
                ImVec2 ts = ImGui::CalcTextSize(buf);
                ImVec2 bp(imgP0.x + 6, imgP0.y + 4);
                dl->AddRectFilled(ImVec2(bp.x - 3, bp.y - 1), ImVec2(bp.x + ts.x + 3, bp.y + ts.y + 1),
                                  IM_COL32(0, 0, 0, 150), 3.0f);
                dl->AddText(bp, IM_COL32(200, 200, 200, 255), buf);
            }

            // label (clipped)
            ImVec4 clip(p0.x, imgP1.y, p1.x, p1.y);
            dl->AddText(nullptr, 0.0f, ImVec2(p0.x + 4, imgP1.y + 2),
                        hovered || selected ? Theme::kTextPrimary : Theme::kTextDim,
                        e.name.c_str(), nullptr, 0.0f, &clip);

            // interactions
            if (hovered && ImGui::IsMouseClicked(ImGuiMouseButton_Left))
                HandleTileClick(idx, io.KeyCtrl, io.KeyShift);
            if (hovered && ImGui::IsMouseDoubleClicked(ImGuiMouseButton_Left))
                OpenInExplorer(idx);
            if (ImGui::IsItemActive() && pendingDrag.empty() &&
                ImGui::IsMouseDragging(ImGuiMouseButton_Left, 6.0f))
                BeginDragOfSelection(idx);

            if (ImGui::BeginPopupContextItem("tilectx")) {
                if (!selection.count(idx)) {
                    selection.clear();
                    selection.insert(idx);
                    lastClicked = idx;
                    SetPreview(idx);
                }
                ImGui::TextDisabled("%s", e.relPath.c_str());
                ImGui::Separator();
                if (ImGui::MenuItem("Copy path")) CopyPathToClipboard(idx);
                if (ImGui::MenuItem("Open in Explorer")) OpenInExplorer(idx);
                if (e.type == AssetType::Texture) {
                    ImGui::Separator();
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
                }
                if (e.type == AssetType::Model && !e.variants.empty()) {
                    ImGui::Separator();
                    for (const std::wstring& v : e.variants) {
                        fs::path vp(v);
                        std::string vext = ToLowerAscii(WideToUtf8(vp.extension().wstring()));
                        std::string item = "Copy path (" + (vext.size() > 1 ? vext.substr(1) : vext) + ")";
                        if (ImGui::MenuItem(item.c_str()))
                            ImGui::SetClipboardText(WideToUtf8(v).c_str());
                    }
                    if (ImGui::BeginMenu("Drag models as")) {
                        auto radio = [&](const char* label, const char* ext) {
                            if (ImGui::MenuItem(label, nullptr, dragFormatExt == ext))
                                dragFormatExt = ext;
                        };
                        radio("Default (GLB preferred)", "");
                        radio("FBX", ".fbx");
                        radio("OBJ (+MTL)", ".obj");
                        radio("DAE", ".dae");
                        ImGui::EndMenu();
                    }
                }
                if (e.type == AssetType::Audio) {
                    ImGui::Separator();
                    if (ImGui::MenuItem(audio.IsPlaying(idx) ? "Stop" : "Play"))
                        audio.Toggle(idx, e.path);
                }
                ImGui::EndPopup();
            }

            if (hovered) {
                ImGui::BeginTooltip();
                ImGui::TextUnformatted(e.relPath.c_str());
                if (e.type == AssetType::Model && !e.variants.empty()) {
                    std::string fmts = "Also available as:";
                    for (const std::wstring& v : e.variants) {
                        fs::path vp(v);
                        fmts += " " + ToLowerAscii(WideToUtf8(vp.extension().wstring()));
                    }
                    ImGui::TextDisabled("%s", fmts.c_str());
                }
                ImGui::TextDisabled("Drag to copy into another app");
                ImGui::EndTooltip();
            }
            ImGui::PopID();
        }
    }

    ImGui::EndChild();
}
