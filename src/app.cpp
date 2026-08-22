#include "app.h"
#include "util.h"
#include "theme.h"
#include "imgui.h"
#include <shlobj.h>
#include <shellapi.h>
#include <filesystem>
#include <chrono>
#include <fstream>

#include "stb_image.h"
#include "stb_image_resize2.h"
#include "stb_image_write.h"

namespace fs = std::filesystem;

std::wstring AppDataDir(const wchar_t* sub) {
    PWSTR base = nullptr;
    std::wstring dir = L".";
    if (SUCCEEDED(SHGetKnownFolderPath(FOLDERID_LocalAppData, 0, nullptr, &base))) {
        dir = base;
        CoTaskMemFree(base);
    }
    return dir + L"\\AssetBrowser\\" + sub;
}

static fs::path RootSettingsPath() {
    return fs::path(AppDataDir(L"")) / L"asset-root.txt";
}

std::wstring LoadSavedAssetRoot() {
    std::ifstream in(RootSettingsPath(), std::ios::binary);
    std::string utf8((std::istreambuf_iterator<char>(in)), std::istreambuf_iterator<char>());
    while (!utf8.empty() && (utf8.back() == '\n' || utf8.back() == '\r')) utf8.pop_back();
    return Utf8ToWide(utf8);
}

static void SaveAssetRoot(const std::wstring& root) {
    std::error_code ec;
    fs::create_directories(RootSettingsPath().parent_path(), ec);
    std::ofstream out(RootSettingsPath(), std::ios::binary | std::ios::trunc);
    out << WideToUtf8(root);
}

static int CALLBACK BrowseCallback(HWND dialog, UINT message, LPARAM, LPARAM data) {
    if (message == BFFM_INITIALIZED && data)
        SendMessageW(dialog, BFFM_SETSELECTIONW, TRUE, data);
    return 0;
}

bool PickAssetRoot(HWND owner, std::wstring& root) {
    BROWSEINFOW info{};
    info.hwndOwner = owner;
    info.lpszTitle = L"Choose the folder containing your asset library";
    info.ulFlags = BIF_RETURNONLYFSDIRS | BIF_NEWDIALOGSTYLE;
    info.lpfn = BrowseCallback;
    info.lParam = (LPARAM)(root.empty() ? nullptr : root.c_str());

    PIDLIST_ABSOLUTE item = SHBrowseForFolderW(&info);
    if (!item) return false;
    wchar_t path[32768];
    bool picked = SHGetPathFromIDListEx(item, path, ARRAYSIZE(path), GPFIDL_DEFAULT) != FALSE;
    CoTaskMemFree(item);
    if (!picked) return false;
    root = path;
    SaveAssetRoot(root);
    return true;
}

// Exported drag files are transient; sweep out anything older than 3 days.
static void SweepOldExports(const std::wstring& dir) {
    std::error_code ec;
    auto cutoff = fs::file_time_type::clock::now() - std::chrono::hours(72);
    for (auto& de : fs::directory_iterator(dir, ec)) {
        auto t = de.last_write_time(ec);
        if (!ec && t < cutoff) fs::remove(de.path(), ec);
    }
}

void App::Init(Gfx* g, HWND wnd, const std::wstring& root) {
    gfx = g;
    hwnd = wnd;
    renderer.Init(*gfx);
    thumbs.Init(gfx, &renderer, AppDataDir(L"thumbs"));
    audio.Init();
    exportDir = AppDataDir(L"export");
    std::error_code ec;
    fs::create_directories(exportDir, ec);
    SweepOldExports(exportDir);
    EmbedInit();
    if (!root.empty()) { db.StartScan(root); EmbedLoadIndexForRoot(); }
}

void App::SetAssetRoot(const std::wstring& root) {
    if (root.empty() || root == db.root) return;
    embedIndexer.Cancel();
    embedIndexer.Join();
    db.StopScan();
    audio.Stop();
    db.entries.clear();
    db.packs.clear();
    db.typeCounts[0] = db.typeCounts[1] = db.typeCounts[2] = 0;
    selection.clear();
    selectedPacks.clear();
    lastClicked = -1;
    previewIndex = -1;
    previewModel.Release();
    previewModelValid = false;
    SafeRelease(previewTexSRV);
    SafeRelease(previewWaveSRV);
    previewPcm = {};
    thumbs.Shutdown();
    thumbs.Init(gfx, &renderer, AppDataDir(L"thumbs"));
    filterDirty = true;
    db.StartScan(root);
    SaveAssetRoot(root);
    SemanticClear();
    EmbedLoadIndexForRoot();
}

void App::Shutdown() {
    EmbedShutdown();
    db.StopScan();
    audio.Shutdown();
    SafeRelease(previewTexSRV);
    SafeRelease(previewWaveSRV);
    previewModel.Release();
    previewRT.Release();
    thumbs.Shutdown();
    renderer.Shutdown();
}

void App::Frame() {
    if (db.PumpScan()) {
        thumbs.EnsureCount(db.entries.size());
        filterDirty = true;
    }
    thumbs.Pump();
    audio.Update();
    PumpEmbed();
    RebuildFilterIfDirty();

    ImGuiIO& io = ImGui::GetIO();
    if (io.KeyCtrl && ImGui::IsKeyPressed(ImGuiKey_F, false)) {
        if (searchHasFocus) selectSearchAll = true;
        else focusSearch = true;
    }
    if (ImGui::IsKeyPressed(ImGuiKey_Space, false) && !io.WantTextInput &&
        previewIndex >= 0 && previewIndex < (int)db.entries.size() &&
        db.entries[previewIndex].type == AssetType::Audio)
        TogglePreviewAudio();

    // Ctrl+V: an image (or copied image file) on the clipboard becomes the
    // query even while the search box is focused, as long as there is no text
    // that the box itself would paste.
    if (io.KeyCtrl && ImGui::IsKeyPressed(ImGuiKey_V, false) && SemanticReady()) {
        bool hasImage = IsClipboardFormatAvailable(CF_DIB) || IsClipboardFormatAvailable(CF_HDROP);
        bool hasText = IsClipboardFormatAvailable(CF_UNICODETEXT);
        if (hasImage && (!io.WantTextInput || !hasText)) PasteImageQuery();
    }
    if (!pendingDropFile.empty()) {
        std::wstring file = std::move(pendingDropFile);
        pendingDropFile.clear();
        if (SemanticReady()) DropFileQuery(file);
    }

    float menuH = 0;
    DrawMenuBar(menuH);
    if (embedWindowOpen) DrawEmbedWindow();

    const float statusH = ImGui::GetFrameHeight() + 6.0f;
    ImGuiViewport* vp = ImGui::GetMainViewport();
    ImGui::SetNextWindowPos(ImVec2(vp->WorkPos.x, vp->WorkPos.y));
    ImGui::SetNextWindowSize(ImVec2(vp->WorkSize.x, vp->WorkSize.y - statusH));
    ImGuiWindowFlags flags = ImGuiWindowFlags_NoTitleBar | ImGuiWindowFlags_NoResize |
                             ImGuiWindowFlags_NoMove | ImGuiWindowFlags_NoCollapse |
                             ImGuiWindowFlags_NoBringToFrontOnFocus | ImGuiWindowFlags_NoNavFocus;
    ImGui::PushStyleVar(ImGuiStyleVar_WindowRounding, 0);
    ImGui::Begin("##main", nullptr, flags);
    ImGui::PopStyleVar();

    const float sidebarW = 230.0f;
    const float splitterW = 8.0f;

    ImGui::BeginChild("sidebar", ImVec2(sidebarW, 0), ImGuiChildFlags_None);
    DrawSidebar();
    ImGui::EndChild();
    ImGui::SameLine();

    float paneW = ImGui::GetContentRegionAvail().x;
    previewRatio = (std::max)(0.2f, (std::min)(previewRatio, 0.6f));
    float previewW = paneW * previewRatio;

    float gridW = paneW - previewW - splitterW -
                  2.0f * ImGui::GetStyle().ItemSpacing.x;
    ImGui::BeginChild("gridpane", ImVec2(gridW, 0), ImGuiChildFlags_None);
    DrawGrid();
    ImGui::EndChild();
    ImGui::SameLine();

    // draggable splitter between grid and preview
    ImGui::InvisibleButton("vsplit", ImVec2(splitterW, ImGui::GetContentRegionAvail().y));
    if (ImGui::IsItemHovered() || ImGui::IsItemActive())
        ImGui::SetMouseCursor(ImGuiMouseCursor_ResizeEW);
    if (ImGui::IsItemActive())
        previewRatio -= ImGui::GetIO().MouseDelta.x / paneW;
    {
        ImVec2 a = ImGui::GetItemRectMin(), b = ImGui::GetItemRectMax();
        float cx = (a.x + b.x) * 0.5f;
        ImU32 col = ImGui::IsItemActive() ? Theme::kSplitterActive
                  : ImGui::IsItemHovered() ? Theme::kSplitterHover
                  : Theme::kSplitterIdle;
        ImGui::GetWindowDrawList()->AddLine(ImVec2(cx, a.y + 4), ImVec2(cx, b.y - 4), col, 2.0f);
    }
    ImGui::SameLine();

    ImGui::BeginChild("preview", ImVec2(0, 0), ImGuiChildFlags_None);
    DrawPreview();
    ImGui::EndChild();

    ImGui::End();

    DrawStatusBar(statusH);
}

void App::DrawMenuBar(float& menuH) {
    menuH = 0;
    if (ImGui::BeginMainMenuBar()) {
        menuH = ImGui::GetWindowHeight();
        if (ImGui::BeginMenu("File")) {
            if (ImGui::MenuItem("Change asset folder...")) {
                requestFolderPicker = true;
            }
            if (ImGui::MenuItem("Rescan assets")) {
                // Only allowed once the previous scan finished
                if (!db.scanning && !embedIndexer.Running()) {
                    embedIndexer.Join();
                    db.StopScan();
                    db.entries.clear();
                    db.packs.clear();
                    db.typeCounts[0] = db.typeCounts[1] = db.typeCounts[2] = 0;
                    selection.clear();
                    previewIndex = -1;
                    previewModel.Release();
                    previewModelValid = false;
                    thumbs.Shutdown();
                    thumbs.Init(gfx, &renderer, AppDataDir(L"thumbs"));
                    filterDirty = true;
                    embedPrunedAfterScan = false;
                    db.StartScan(db.root);
                }
            }
            if (ImGui::MenuItem("Clear thumbnail cache")) {
                thumbs.ClearCacheDisk();
            }
            if (ImGui::MenuItem("Open export folder")) {
                ShellExecuteW(nullptr, L"open", exportDir.c_str(), nullptr, nullptr, SW_SHOWNORMAL);
            }
            ImGui::Separator();
            if (ImGui::MenuItem("Exit", "Alt+F4"))
                PostMessageW(hwnd, WM_CLOSE, 0, 0);
            ImGui::EndMenu();
        }
        if (ImGui::BeginMenu("View")) {
            ImGui::SetNextItemWidth(160);
            ImGui::SliderFloat("Tile size", &tileSize, 72.0f, 220.0f, "%.0f px");
            ImGui::EndMenu();
        }
        if (ImGui::BeginMenu("Tools")) {
            if (ImGui::MenuItem("Embedding search...", nullptr, embedWindowOpen))
                embedWindowOpen = !embedWindowOpen;
            ImGui::EndMenu();
        }
        ImGui::EndMainMenuBar();
    }
}

void App::DrawSidebar() {
    // Nothing selected in a group means that group doesn't filter.
    ImGui::SeparatorText("Type");
    const char* names[3] = { "Textures", "Models", "Audio" };
    for (int i = 0; i < 3; ++i) {
        char label[64];
        snprintf(label, sizeof(label), "%s (%d)##type%d", names[i], db.typeCounts[i], i);
        if (typeSel[i]) ImGui::PushStyleColor(ImGuiCol_Text, Theme::Accent());
        bool clicked = ImGui::Selectable(label, typeSel[i]);
        if (typeSel[i]) ImGui::PopStyleColor();
        if (clicked) {
            const ImGuiIO& io = ImGui::GetIO();
            if (io.KeyAlt) {
                typeSel[0] = typeSel[1] = typeSel[2] = false;
            } else if (io.KeyShift && lastTypeClicked >= 0) {
                if (!io.KeyCtrl)
                    typeSel[0] = typeSel[1] = typeSel[2] = false;
                int a = std::min(lastTypeClicked, i);
                int b = std::max(lastTypeClicked, i);
                for (int j = a; j <= b; ++j) typeSel[j] = true;
            } else if (io.KeyCtrl) {
                typeSel[i] = !typeSel[i];
                lastTypeClicked = i;
            } else {
                bool wasSelected = typeSel[i];
                typeSel[0] = typeSel[1] = typeSel[2] = false;
                typeSel[i] = !wasSelected;
                lastTypeClicked = i;
            }
            thumbs.FlushPending();
            filterDirty = true;
        }
    }

    ImGui::SeparatorText("Packs");
    ImGui::BeginChild("packs", ImVec2(0, 0), ImGuiChildFlags_None);
    for (size_t i = 0; i < db.packs.size(); ++i) {
        const PackInfo& p = db.packs[i];
        char label[512];
        snprintf(label, sizeof(label), "%s (%d)", p.name.c_str(), p.count);
        bool on = selectedPacks.count(p.name) > 0;
        if (on) ImGui::PushStyleColor(ImGuiCol_Text, Theme::Accent());
        bool clicked = ImGui::Selectable(label, on, ImGuiSelectableFlags_SpanAllColumns);
        if (on) ImGui::PopStyleColor();
        if (clicked) {
            const ImGuiIO& io = ImGui::GetIO();
            auto anchor = std::find_if(db.packs.begin(), db.packs.end(), [&](const PackInfo& candidate) {
                return candidate.name == lastPackClicked;
            });
            if (io.KeyAlt) {
                selectedPacks.clear();
            } else if (io.KeyShift && anchor != db.packs.end()) {
                if (!io.KeyCtrl) selectedPacks.clear();
                size_t anchorIndex = (size_t)std::distance(db.packs.begin(), anchor);
                size_t a = std::min(anchorIndex, i);
                size_t b = std::max(anchorIndex, i);
                for (size_t j = a; j <= b; ++j) selectedPacks.insert(db.packs[j].name);
            } else if (io.KeyCtrl) {
                if (on) selectedPacks.erase(p.name);
                else selectedPacks.insert(p.name);
                lastPackClicked = p.name;
            } else {
                selectedPacks.clear();
                if (!on) selectedPacks.insert(p.name);
                lastPackClicked = p.name;
            }
            thumbs.FlushPending();
            filterDirty = true;
        }
    }
    ImGui::EndChild();
}

void App::DrawStatusBar(float h) {
    ImGuiViewport* vp = ImGui::GetMainViewport();
    ImGui::SetNextWindowPos(ImVec2(vp->WorkPos.x, vp->WorkPos.y + vp->WorkSize.y - h));
    ImGui::SetNextWindowSize(ImVec2(vp->WorkSize.x, h));
    ImGuiWindowFlags flags = ImGuiWindowFlags_NoTitleBar | ImGuiWindowFlags_NoResize |
                             ImGuiWindowFlags_NoMove | ImGuiWindowFlags_NoScrollbar |
                             ImGuiWindowFlags_NoSavedSettings;
    ImGui::PushStyleVar(ImGuiStyleVar_WindowRounding, 0);
    ImGui::Begin("##status", nullptr, flags);
    ImGui::PopStyleVar();

    if (db.root.empty())
        ImGui::Text("No asset folder selected");
    else if (db.scanning)
        ImGui::Text("Scanning...  %d assets found", (int)db.entries.size());
    else
        ImGui::Text("%d assets", (int)db.entries.size());
    ImGui::SameLine(0, 24);
    if (totalFilterMatches > filtered.size())
        ImGui::Text("Showing top %d of %zu", (int)filtered.size(), totalFilterMatches);
    else
        ImGui::Text("Showing %d", (int)filtered.size());
    int pending = thumbs.PendingJobs();
    if (pending > 0) {
        ImGui::SameLine(0, 24);
        ImGui::Text("Generating thumbnails: %d queued", pending);
    }
    if (embedIndexer.Running()) {
        ImGui::SameLine(0, 24);
        ImGui::TextColored(Theme::Accent(0.9f), "Embedding: %d / %d", embedIndexer.done.load(), embedIndexer.total.load());
    }
    if (SemanticActive()) {
        ImGui::SameLine(0, 24);
        ImGui::TextColored(Theme::Accent(0.9f), "Semantic: %s", semanticLabel.c_str());
    }
    if (!selection.empty()) {
        ImGui::SameLine(0, 24);
        ImGui::Text("Selected: %d", (int)selection.size());
    }
    // active drag-export modes, right-aligned
    std::string mode;
    if (texDragOpt == -2) mode = "textures drag at 1/2";
    else if (texDragOpt == -4) mode = "textures drag at 1/4";
    else if (texDragOpt > 0) mode = "textures drag at max " + std::to_string(texDragOpt) + "px";
    if (!dragFormatExt.empty()) {
        if (!mode.empty()) mode += "   ";
        mode += "models drag as " + dragFormatExt.substr(1);
    }
    if (!mode.empty()) {
        float w = ImGui::CalcTextSize(mode.c_str()).x;
        ImGui::SameLine(ImGui::GetWindowWidth() - w - 16);
        ImGui::TextColored(Theme::Accent(0.9f), "%s", mode.c_str());
    }
    ImGui::End();
}

void App::HandleTileClick(int index, bool ctrl, bool shift) {
    if (shift && lastClicked >= 0) {
        // range select within current filtered order
        int a = -1, b = -1;
        for (int i = 0; i < (int)filtered.size(); ++i) {
            if (filtered[i] == lastClicked) a = i;
            if (filtered[i] == index) b = i;
        }
        if (a >= 0 && b >= 0) {
            if (!ctrl) selection.clear();
            if (a > b) std::swap(a, b);
            for (int i = a; i <= b; ++i) selection.insert(filtered[i]);
        }
    } else if (ctrl) {
        if (selection.count(index)) selection.erase(index);
        else selection.insert(index);
        lastClicked = index;
    } else {
        selection.clear();
        selection.insert(index);
        lastClicked = index;
    }
    SetPreview(index);
    if (index >= 0 && index < (int)db.entries.size() &&
        db.entries[index].type == AssetType::Audio && !ctrl && !shift)
        audio.Toggle(index, db.entries[index].path);
}

// Applies the texture drag-size setting to (w, h); returns true if it shrinks.
bool App::DragDims(int w, int h, int& nw, int& nh) const {
    nw = w; nh = h;
    if (texDragOpt == -2) { nw = w / 2; nh = h / 2; }
    else if (texDragOpt == -4) { nw = w / 4; nh = h / 4; }
    else if (texDragOpt > 0 && (w > texDragOpt || h > texDragOpt)) {
        float s = (std::min)((float)texDragOpt / w, (float)texDragOpt / h);
        nw = (int)(w * s); nh = (int)(h * s);
    }
    nw = (std::max)(1, nw); nh = (std::max)(1, nh);
    return nw < w || nh < h;
}

// Decode + downscale a texture into the export dir; returns the file to drag,
// or empty if the original should be dragged (no downscale needed / failure).
std::wstring App::ExportResizedTexture(const AssetEntry& e) {
    if (texDragOpt == 0) return L"";
    std::vector<uint8_t> bytes;
    if (!ReadFileBytes(e.path, bytes) || bytes.empty()) return L"";
    int w = 0, h = 0, comp = 0;
    stbi_uc* px = stbi_load_from_memory(bytes.data(), (int)bytes.size(), &w, &h, &comp, 4);
    if (!px) return L"";

    int nw = 0, nh = 0;
    if (!DragDims(w, h, nw, nh)) { stbi_image_free(px); return L""; } // nothing to shrink

    std::vector<uint8_t> resized((size_t)nw * nh * 4);
    stbir_resize_uint8_srgb(px, w, h, w * 4, resized.data(), nw, nh, nw * 4, STBIR_RGBA);
    stbi_image_free(px);

    wchar_t suffix[48];
    swprintf_s(suffix, L"_%dx%d.png", nw, nh);
    std::wstring outPath = exportDir + L"\\" + Utf8ToWide(e.name) + suffix;

    // reuse a previous export if the source hasn't changed since
    std::error_code ec;
    if (fs::exists(outPath, ec)) return outPath;

    if (!stbi_write_png(WideToUtf8(outPath).c_str(), nw, nh, 4, resized.data(), nw * 4))
        return L"";
    return outPath;
}

std::wstring App::ExportAudioSelection() {
    if (previewIndex < 0 || previewIndex >= (int)db.entries.size()) return L"";
    const AssetEntry& e = db.entries[previewIndex];
    if (e.type != AssetType::Audio || !previewPcm.valid) return L"";

    uint64_t f0 = 0, f1 = 0;
    bool hasSel = AudioSelectionFrames(f0, f1);

    wchar_t suffix[80];
    if (hasSel) {
        double sr = (double)previewPcm.sampleRate;
        swprintf_s(suffix, audioTuneLoop ? L"_%.0f-%.0fms_loop.wav" : L"_%.0f-%.0fms.wav",
                   f0 / sr * 1000.0, f1 / sr * 1000.0);
    } else {
        wcscpy_s(suffix, audioTuneLoop ? L"_loop.wav" : L".wav");
    }
    std::wstring outPath = exportDir + L"\\" + Utf8ToWide(e.name) + suffix;
    if (audioTuneLoop) {
        PcmData loop = BuildTunedAudioLoop();
        if (!loop.valid || !WriteWav16(outPath, loop, 0, loop.frames)) return L"";
    } else if (!WriteWav16(outPath, previewPcm, f0, f1)) {
        return L"";
    }
    return outPath;
}

void App::TogglePreviewAudio() {
    if (previewIndex < 0 || previewIndex >= (int)db.entries.size() || !previewPcm.valid) return;
    const AssetEntry& e = db.entries[previewIndex];
    if (e.type != AssetType::Audio) return;
    if (audio.IsPlaying(previewIndex)) { audio.Stop(); return; }

    uint64_t f0 = 0, f1 = 0;
    bool hasSel = AudioSelectionFrames(f0, f1);
    if (audioTuneLoop) {
        PcmData loop = BuildTunedAudioLoop();
        if (loop.valid) audio.PlayLoop(previewIndex, std::move(loop));
    } else if (hasSel) {
        float startSec = (float)f0 / previewPcm.sampleRate;
        float endSec = (float)f1 / previewPcm.sampleRate;
        audio.PlayRange(previewIndex, e.path, startSec, endSec);
    } else {
        audio.Toggle(previewIndex, e.path);
    }
}

bool App::AudioSelectionFrames(uint64_t& f0, uint64_t& f1) const {
    float a = audioSelA, b = audioSelB;
    if (a > b) std::swap(a, b);
    bool hasSel = (a >= 0.0f && (b - a) > 0.001f);
    f0 = hasSel ? (uint64_t)((double)a * previewPcm.frames) : 0;
    f1 = hasSel ? (uint64_t)((double)b * previewPcm.frames) : previewPcm.frames;
    return hasSel;
}

uint64_t App::AudioLoopCrossfadeFrames(uint64_t f0, uint64_t f1) const {
    uint64_t frames = (uint64_t)((double)audioLoopCrossfadeMs * previewPcm.sampleRate / 1000.0 + 0.5);
    return (std::min)(frames, (f1 - f0) / 2);
}

PcmData App::BuildTunedAudioLoop() const {
    uint64_t f0 = 0, f1 = 0;
    AudioSelectionFrames(f0, f1);
    uint64_t crossfadeFrames = AudioLoopCrossfadeFrames(f0, f1);
    return BuildSeamlessLoop(previewPcm, f0, f1, crossfadeFrames);
}

std::vector<std::wstring> App::DragPathsFor(int index) {
    std::vector<std::wstring> out;
    if (index < 0 || index >= (int)db.entries.size()) return out;
    const AssetEntry& e = db.entries[index];

    if (e.type == AssetType::Texture) {
        std::wstring exported = ExportResizedTexture(e);
        out.push_back(exported.empty() ? e.path : exported);
        return out;
    }

    // Models: honor the preferred drag format if that variant exists
    std::wstring chosen = e.path;
    if (e.type == AssetType::Model && !dragFormatExt.empty() && e.ext != dragFormatExt) {
        std::wstring want = Utf8ToWide(dragFormatExt);
        for (const std::wstring& v : e.variants) {
            std::wstring lower = ToLowerAsciiW(v);
            if (lower.size() >= want.size() &&
                lower.compare(lower.size() - want.size(), want.size(), want) == 0) {
                chosen = v;
                break;
            }
        }
    }
    out.push_back(chosen);

    // OBJ needs its material library alongside
    fs::path cp(chosen);
    if (ToLowerAsciiW(cp.extension().wstring()) == L".obj") {
        fs::path mtl = cp;
        mtl.replace_extension(L".mtl");
        std::error_code ec;
        if (fs::exists(mtl, ec)) out.push_back(mtl.wstring());
    }
    return out;
}

void App::BeginDragOfSelection(int draggedIndex) {
    pendingDrag.clear();
    std::vector<int> indices;
    if (selection.count(draggedIndex))
        indices.assign(selection.begin(), selection.end());
    else
        indices.push_back(draggedIndex);
    for (int i : indices)
        for (auto& p : DragPathsFor(i))
            pendingDrag.push_back(std::move(p));
}

void App::OpenInExplorer(int index) {
    if (index < 0 || index >= (int)db.entries.size()) return;
    PIDLIST_ABSOLUTE pidl = ILCreateFromPathW(db.entries[index].path.c_str());
    if (pidl) {
        SHOpenFolderAndSelectItems(pidl, 0, nullptr, 0);
        ILFree(pidl);
    }
}

void App::CopyPathToClipboard(int index) {
    if (index < 0 || index >= (int)db.entries.size()) return;
    ImGui::SetClipboardText(WideToUtf8(db.entries[index].path).c_str());
}
