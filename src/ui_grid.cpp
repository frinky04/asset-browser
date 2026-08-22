#include "app.h"
#include "util.h"
#include "theme.h"
#include "imgui.h"
#include <filesystem>
#include <algorithm>
#include <cctype>
#include <cstring>

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

int SearchInputCallback(ImGuiInputTextCallbackData* data) {
    bool* selectAll = static_cast<bool*>(data->UserData);
    if (*selectAll) {
        data->SelectAll();
        *selectAll = false;
    }
    return 0;
}

void SetQuery(char* buffer, size_t size, const std::string& query) {
    strncpy_s(buffer, size, query.c_str(), _TRUNCATE);
}

void ToggleExtensionQuery(char* buffer, size_t size, const std::string& extension) {
    std::string input = buffer;
    std::string output;
    bool removed = false;
    for (size_t pos = 0; pos < input.size();) {
        while (pos < input.size() && std::isspace((unsigned char)input[pos])) ++pos;
        if (pos == input.size()) break;
        size_t end = input.find_first_of(" \t\r\n", pos);
        if (end == std::string::npos) end = input.size();
        std::string token = input.substr(pos, end - pos);
        if (ToLowerAscii(token).starts_with("ext:")) {
            std::string kept;
            size_t valuePos = 4;
            while (valuePos <= token.size()) {
                size_t comma = token.find(',', valuePos);
                if (comma == std::string::npos) comma = token.size();
                std::string value = token.substr(valuePos, comma - valuePos);
                if (ToLowerAscii(value) == extension) removed = true;
                else {
                    if (!kept.empty()) kept += ',';
                    kept += value;
                }
                valuePos = comma + 1;
            }
            token = kept.empty() ? "" : "ext:" + kept;
        }
        if (!token.empty()) {
            if (!output.empty()) output += ' ';
            output += token;
        }
        pos = end;
    }
    if (!removed) {
        if (!output.empty()) output += ' ';
        output += "ext:" + extension;
    }
    SetQuery(buffer, size, output);
}

void SelectResult(App& app, int slot) {
    if (app.filtered.empty()) return;
    slot = (std::max)(0, (std::min)(slot, (int)app.filtered.size() - 1));
    int index = app.filtered[slot];
    app.selection.clear();
    app.selection.insert(index);
    app.lastClicked = index;
    app.scrollToIndex = index;
    app.SetPreview(index);
}

} // namespace

void App::DrawGrid() {
    if (db.root.empty()) {
        ImGui::BeginChild("empty-grid", ImVec2(0, 0), ImGuiChildFlags_None);
        ImVec2 avail = ImGui::GetContentRegionAvail();
        ImGui::SetCursorPosY((std::max)(24.0f, avail.y * 0.35f));

        const char* title = "No asset folder selected";
        float titleW = ImGui::CalcTextSize(title).x;
        ImGui::SetCursorPosX((std::max)(0.0f, (avail.x - titleW) * 0.5f));
        ImGui::TextUnformatted(title);

        ImGui::Dummy(ImVec2(0, 8.0f));
        const char* button = "Choose asset folder...";
        float buttonW = ImGui::CalcTextSize(button).x + ImGui::GetStyle().FramePadding.x * 2.0f;
        ImGui::SetCursorPosX((std::max)(0.0f, (avail.x - buttonW) * 0.5f));
        if (ImGui::Button(button)) requestFolderPicker = true;
        ImGui::EndChild();
        return;
    }

    // --- search bar ---
    if (focusSearch) {
        ImGui::SetKeyboardFocusHere();
        selectSearchAll = true;
        focusSearch = false;
    }
    float clearW = ImGui::CalcTextSize("Clear").x + ImGui::GetStyle().FramePadding.x * 2.0f;
    float sortW = 122.0f;
    bool semanticAvail = SemanticReady();
    bool semanticMode = semanticSearch && semanticAvail;
    bool showSemanticBox = semanticAvail || !embedKey.empty();
    float semanticW = showSemanticBox ? ImGui::CalcTextSize("Semantic").x + ImGui::GetFrameHeight() +
                                        ImGui::GetStyle().ItemInnerSpacing.x + ImGui::GetStyle().ItemSpacing.x : 0.0f;
    ImGui::SetNextItemWidth(ImGui::GetContentRegionAvail().x - clearW - sortW - semanticW - ImGui::GetStyle().ItemSpacing.x * 2.0f);
    if (semanticMode) ImGui::PushStyleColor(ImGuiCol_Border, Theme::Accent(0.7f));
    const char* hint = semanticMode ? "Describe a texture and press Enter...  (Ctrl+F)" : "Search assets...  (Ctrl+F)";
    if (ImGui::InputTextWithHint("##search", hint, searchBuf, sizeof(searchBuf),
                                 ImGuiInputTextFlags_AutoSelectAll | ImGuiInputTextFlags_EscapeClearsAll |
                                 ImGuiInputTextFlags_CallbackAlways,
                                 SearchInputCallback, &selectSearchAll)) {
        // Semantic mode searches on Enter only: each query is a paid request and
        // live fuzzy results would flicker in underneath the ranking.
        if (!semanticMode) {
            thumbs.FlushPending();
            filterDirty = true;
        }
    }
    if (semanticMode) ImGui::PopStyleColor();
    searchHasFocus = ImGui::IsItemActive();
    // Esc on an already-empty box drops the semantic ranking too.
    if (ImGui::IsItemDeactivated() && ImGui::IsKeyPressed(ImGuiKey_Escape, false) && !searchBuf[0] && SemanticActive()) {
        SemanticClear();
        thumbs.FlushPending();
        filterDirty = true;
    }
    // Enter deactivates a single-line InputText in the same frame, so the box
    // reads as inactive by the time we check the key.
    bool searchEnter = (searchHasFocus || ImGui::IsItemDeactivated()) &&
                       (ImGui::IsKeyPressed(ImGuiKey_Enter, false) || ImGui::IsKeyPressed(ImGuiKey_KeypadEnter, false));
    if (searchHasFocus && (ImGui::IsKeyPressed(ImGuiKey_DownArrow, false) ||
                           (ImGui::GetIO().KeyCtrl && ImGui::IsKeyPressed(ImGuiKey_N, false)))) {
        int slot = 0;
        if (!selection.empty()) {
            auto it = std::find(filtered.begin(), filtered.end(), *selection.begin());
            if (it != filtered.end()) slot = (int)(it - filtered.begin()) + 1;
        }
        SelectResult(*this, slot);
    }
    if (searchEnter) {
        std::string semanticText = semanticMode ? SemanticTextFromQuery(ParseQueryText(searchBuf)) : "";
        if (semanticMode && semanticBusy) {
            // a query is already in flight; ignore
        } else if (!semanticText.empty()) {
            SemanticQueryText(semanticText); // Enter runs the (paid) text embedding; typing never does
        } else if (semanticMode && SemanticActive()) {
            thumbs.FlushPending();
            filterDirty = true;              // re-apply filter terms to the current image/similar ranking
        } else {
            bool selectedResult = !selection.empty() &&
                std::find(filtered.begin(), filtered.end(), *selection.begin()) != filtered.end();
            if (!selectedResult) SelectResult(*this, 0);
            focusGrid = true;
        }
    }
    if (showSemanticBox) {
        ImGui::SameLine();
        ImGui::BeginDisabled(!semanticAvail);
        if (ImGui::Checkbox("Semantic", &semanticSearch)) {
            thumbs.FlushPending();
            filterDirty = true;
            if (semanticSearch) focusSearch = true;
        }
        ImGui::EndDisabled();
        if (ImGui::IsItemHovered(ImGuiHoveredFlags_AllowWhenDisabled)) {
            if (semanticAvail)
                ImGui::SetTooltip("Rank indexed textures, models and sounds by meaning.\nDescribe one and press Enter, Ctrl+V / drop an image\n"
                                  "or sound file, or select an asset and press S.");
            else
                ImGui::SetTooltip("Index your assets first: Semantic > Embedding settings & indexing.");
        }
    }
    ImGui::SameLine();
    if (ImGui::Button("Clear") && (searchBuf[0] || SemanticActive() || semanticBusy)) {
        searchBuf[0] = 0;
        SemanticClear();
        thumbs.FlushPending();
        filterDirty = true;
    }
    if (ImGui::IsItemHovered() && SemanticActive()) ImGui::SetTooltip("Clear the text and the semantic query");
    ImGui::SameLine();
    ImGui::SetNextItemWidth(sortW);
    static const char* sortNames[] = { "Relevance", "Name", "Newest", "Largest" };
    static const char* sortNamesSemantic[] = { "Similarity", "Name", "Newest", "Largest" };
    if (ImGui::Combo("##sort", &sortMode, SemanticActive() ? sortNamesSemantic : sortNames, IM_ARRAYSIZE(sortNames)))
        filterDirty = true;
    if (!searchBuf[0] && sortMode == 0 && ImGui::IsItemHovered())
        ImGui::SetTooltip("Relevance applies when searching");

    // Sidebar and search changes happen during this frame, after App::Frame's
    // normal filter rebuild. Apply them before requesting visible thumbnails.
    RebuildFilterIfDirty();

    // --- semantic query chip: what the grid is currently ranked against ---
    if (semanticMode || semanticBusy) {
        const float chipH = ImGui::GetFrameHeight();
        if (semanticBusy) {
            const char* dots[] = { "   ", ".  ", ".. ", "..." };
            int phase = (int)(ImGui::GetTime() * 4.0) % 4;
            ImGui::AlignTextToFramePadding();
            ImGui::TextColored(Theme::Accent(), "Embedding query%s", dots[phase]);
        } else if (SemanticActive()) {
            ImGui::BeginGroup();
            // query image: pasted / dropped, or the source texture of "find similar"
            ID3D11ShaderResourceView* srv = semanticImageSRV;
            int iw = semanticImageW, ih = semanticImageH;
            if (semanticKind == SemanticKind::Asset && semanticAssetIndex >= 0 &&
                semanticAssetIndex < (int)db.entries.size()) {
                thumbs.Request(semanticAssetIndex, db.entries[semanticAssetIndex]);
                const Thumb& t = thumbs.Get(semanticAssetIndex);
                if (t.state == ThumbState::Ready && t.srv) { srv = t.srv; iw = t.w; ih = t.h; }
            }
            const float side = chipH * 2.0f;
            if (srv && iw > 0 && ih > 0) {
                float sc = (std::min)(side / iw, side / ih);
                ImGui::Image((ImTextureID)(intptr_t)srv, ImVec2(iw * sc, ih * sc));
                if (ImGui::IsItemHovered()) {
                    ImGui::BeginTooltip();
                    float bs = (std::min)(256.0f / iw, 256.0f / ih);
                    ImGui::Image((ImTextureID)(intptr_t)srv, ImVec2(iw * bs, ih * bs));
                    if (semanticKind == SemanticKind::Asset) ImGui::TextDisabled("Click to select the source texture");
                    else ImGui::TextDisabled("%dx%d query image", iw, ih);
                    ImGui::EndTooltip();
                }
                if (semanticKind == SemanticKind::Asset && ImGui::IsItemClicked()) {
                    selection.clear(); selection.insert(semanticAssetIndex);
                    scrollToIndex = semanticAssetIndex; SetPreview(semanticAssetIndex);
                }
                ImGui::SameLine();
            }
            ImGui::BeginGroup();
            const char* kindName = semanticKind == SemanticKind::Text ? "Ranked by description"
                                 : semanticKind == SemanticKind::Image ? "Ranked by image"
                                 : semanticKind == SemanticKind::Audio ? "Ranked by sound" : "Ranked by similarity";
            ImGui::TextColored(Theme::Accent(), "%s", kindName);
            if (semanticKind == SemanticKind::Text) ImGui::TextDisabled("\"%s\"", semanticLabel.c_str());
            else ImGui::TextDisabled("%s", semanticLabel.c_str());
            ImGui::EndGroup();
            ImGui::SameLine();
            if (ImGui::SmallButton("x##clearsem")) {
                SemanticClear();
                thumbs.FlushPending();
                filterDirty = true;
            }
            if (ImGui::IsItemHovered()) ImGui::SetTooltip("Clear the semantic query (keeps the text)");
            ImGui::EndGroup();
        } else {
            ImGui::AlignTextToFramePadding();
            ImGui::TextDisabled("Describe an asset and press Enter, Ctrl+V / drop an image or sound file, or select an asset and press S.");
        }
        if (embedUnindexedCount > 0 && !embedIndexer.Running()) {
            ImGui::TextDisabled("%d asset%s not indexed yet and won't appear in rankings.", embedUnindexedCount,
                                embedUnindexedCount == 1 ? " is" : "s are");
            ImGui::SameLine();
            if (ImGui::SmallButton("Index now...")) embedWindowOpen = true;
        }
        if (!semanticError.empty())
            ImGui::TextColored(ImVec4(1.0f, 0.45f, 0.45f, 1.0f), "%s", semanticError.c_str());
    }
    if (!semanticNotice.empty()) {
        if (ImGui::GetTime() > semanticNoticeUntil) semanticNotice.clear();
        else ImGui::TextColored(Theme::Accent(0.9f), "%s", semanticNotice.c_str());
    }

    if (SemanticActive())
        ImGui::TextDisabled("%zu indexed assets ranked", totalFilterMatches);
    else
        ImGui::TextDisabled("%zu results", totalFilterMatches);
    static const char* typeNames[] = { "textures", "models", "audio" };
    for (int i = 0; i < 3; ++i) {
        ImGui::SameLine(0, 8);
        ImGui::TextColored(typeSel[i] ? Theme::Accent() : ImGui::GetStyleColorVec4(ImGuiCol_TextDisabled),
                           "\xC2\xB7 %d %s", resultTypeCounts[i], typeNames[i]);
        if (ImGui::IsItemClicked()) {
            typeSel[i] = !typeSel[i];
            filterDirty = true;
        }
    }

    std::vector<std::pair<std::string, int>> extensions(resultExtCounts.begin(), resultExtCounts.end());
    std::sort(extensions.begin(), extensions.end(), [](const auto& a, const auto& b) {
        return a.second != b.second ? a.second > b.second : a.first < b.first;
    });
    int shownExtensions = (std::min)(8, (int)extensions.size());
    for (int i = 0; i < shownExtensions; ++i) {
        if (i) ImGui::SameLine();
        std::string label = extensions[i].first + "  " + std::to_string(extensions[i].second);
        ImGui::PushID(extensions[i].first.c_str());
        if (ImGui::Button(label.c_str())) {
            ToggleExtensionQuery(searchBuf, sizeof(searchBuf), extensions[i].first);
            thumbs.FlushPending();
            filterDirty = true;
        }
        ImGui::PopID();
    }
    if (shownExtensions) ImGui::Dummy(ImVec2(0, 2.0f));
    RebuildFilterIfDirty();

    // --- virtualized tile grid ---
    ImGui::BeginChild("grid", ImVec2(0, 0), ImGuiChildFlags_None);
    if (focusGrid) {
        ImGui::SetWindowFocus();
        focusGrid = false;
    }
    ImGuiIO& io = ImGui::GetIO();
    if (ImGui::IsWindowHovered() && io.KeyCtrl && io.MouseWheel != 0.0f)
        tileSize = (std::max)(72.0f, (std::min)(tileSize + io.MouseWheel * 8.0f, 220.0f));

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

    if (ImGui::IsWindowFocused() && !filtered.empty()) {
        int slot = 0;
        if (!selection.empty()) {
            auto it = std::find(filtered.begin(), filtered.end(), *selection.begin());
            if (it != filtered.end()) slot = (int)(it - filtered.begin());
        }
        if (ImGui::IsKeyPressed(ImGuiKey_S, false) && !io.KeyCtrl && !selection.empty())
            SemanticFindSimilar(*selection.begin());
        int next = slot;
        if (ImGui::IsKeyPressed(ImGuiKey_LeftArrow, false)) --next;
        if (ImGui::IsKeyPressed(ImGuiKey_RightArrow, false)) ++next;
        if (ImGui::IsKeyPressed(ImGuiKey_UpArrow, false)) next -= cols;
        if (ImGui::IsKeyPressed(ImGuiKey_DownArrow, false)) next += cols;
        if (next != slot) SelectResult(*this, next);
    }

    if (scrollToIndex >= 0) {
        auto it = std::find(filtered.begin(), filtered.end(), scrollToIndex);
        if (it != filtered.end()) ImGui::SetScrollY(((int)(it - filtered.begin()) / cols) * cellH);
        scrollToIndex = -1;
    }

    ImGui::Dummy(ImVec2(0, rows * cellH)); // establish scroll extent

    float scrollY = ImGui::GetScrollY();
    float viewH = ImGui::GetWindowHeight();
    int firstRow = (std::max)(0, (int)(scrollY / cellH) - 1);
    int lastRow = (std::min)(rows - 1, (int)((scrollY + viewH) / cellH) + 1);

    ImDrawList* dl = ImGui::GetWindowDrawList();

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

            // similarity badge (semantic ranking)
            if (SemanticActive() && idx < (int)semanticScore.size()) {
                char buf[16];
                snprintf(buf, sizeof(buf), "%.2f", semanticScore[idx]);
                ImVec2 ts = ImGui::CalcTextSize(buf);
                ImVec2 bp(imgP1.x - ts.x - 7, imgP0.y + 4);
                dl->AddRectFilled(ImVec2(bp.x - 3, bp.y - 1), ImVec2(bp.x + ts.x + 3, bp.y + ts.y + 1),
                                  IM_COL32(0, 0, 0, 160), 3.0f);
                dl->AddText(bp, slot == 0 ? Theme::kAccent : IM_COL32(210, 210, 210, 255), buf);
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
            ImVec2 labelPos(p0.x + 4, imgP1.y + 2);
            dl->AddText(nullptr, 0.0f, labelPos,
                        hovered || selected ? Theme::kTextPrimary : Theme::kTextDim,
                        e.name.c_str(), nullptr, 0.0f, &clip);
            std::vector<int> matchPositions;
            if (SearchNameMatchPositions(parsedQuery, e.nameLower, matchPositions)) {
                for (int pos : matchPositions) {
                    const char* begin = e.name.c_str() + pos;
                    if (!*begin) continue;
                    float x = ImGui::CalcTextSize(e.name.c_str(), begin).x;
                    dl->AddText(nullptr, 0.0f, ImVec2(labelPos.x + x, labelPos.y), Theme::kAccent,
                                begin, begin + 1, 0.0f, &clip);
                }
            }

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
                    if (SemanticReady()) {
                        bool indexed = embedIndex.VectorFor(e) != nullptr;
                        ImGui::BeginDisabled(!indexed);
                        if (ImGui::MenuItem("Find similar textures", "S")) SemanticQueryAsset(idx);
                        ImGui::EndDisabled();
                        if (!indexed && ImGui::IsItemHovered(ImGuiHoveredFlags_AllowWhenDisabled))
                            ImGui::SetTooltip("Not in the embedding index");
                    }
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
                if (e.type == AssetType::Model && SemanticReady()) {
                    ImGui::Separator();
                    bool indexed = embedIndex.VectorFor(e) != nullptr;
                    ImGui::BeginDisabled(!indexed);
                    if (ImGui::MenuItem("Find similar models", "S")) SemanticQueryAsset(idx);
                    ImGui::EndDisabled();
                    if (!indexed && ImGui::IsItemHovered(ImGuiHoveredFlags_AllowWhenDisabled))
                        ImGui::SetTooltip(EmbedModelRenderable(e.ext) ? "Not in the embedding index" : "This format cannot be rendered, so it is not indexed");
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
                    if (SemanticReady()) {
                        bool indexed = embedIndex.VectorFor(e) != nullptr;
                        ImGui::BeginDisabled(!indexed);
                        if (ImGui::MenuItem("Find similar sounds", "S")) SemanticQueryAsset(idx);
                        ImGui::EndDisabled();
                        if (!indexed && ImGui::IsItemHovered(ImGuiHoveredFlags_AllowWhenDisabled))
                            ImGui::SetTooltip("Not in the embedding index");
                    }
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
                if (SemanticActive() && idx < (int)semanticScore.size())
                    ImGui::TextDisabled("Similarity %.3f", semanticScore[idx]);
                ImGui::TextDisabled("Drag to copy into another app");
                ImGui::EndTooltip();
            }
            ImGui::PopID();
        }
    }

    ImGui::EndChild();
}
