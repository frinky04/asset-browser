// Embedding search: settings/indexing window, semantic query plumbing, clipboard paste.
#include "app.h"
#include "util.h"
#include "theme.h"
#include "imgui.h"
#include <shellapi.h>
#include <filesystem>
#include <cmath>
#include <chrono>

#include "stb_image.h"
#include "stb_image_resize2.h"
#include <nlohmann/json.hpp>
#include <fstream>

namespace fs = std::filesystem;

namespace {


std::wstring SettingsFile() { return EmbedSettingsFile(); }
std::wstring KeyFile() { return EmbedKeyFile(); }

double NowSeconds() {
    return std::chrono::duration<double>(std::chrono::steady_clock::now().time_since_epoch()).count();
}

// Fit RGBA into a max side (thumbnail-sized query image).
void FitRgba(const uint8_t* rgba, int w, int h, int maxSide, std::vector<uint8_t>& out, int& ow, int& oh) {
    if (w <= maxSide && h <= maxSide) {
        ow = w; oh = h; out.assign(rgba, rgba + (size_t)w * h * 4);
        return;
    }
    float s = (std::min)((float)maxSide / w, (float)maxSide / h);
    ow = (std::max)(1, (int)(w * s)); oh = (std::max)(1, (int)(h * s));
    out.resize((size_t)ow * oh * 4);
    stbir_resize_uint8_srgb(rgba, w, h, w * 4, out.data(), ow, oh, ow * 4, STBIR_RGBA);
}

// Clipboard: CF_DIB bitmap, else first CF_HDROP file decoded with stb.
bool ReadClipboardImage(HWND hwnd, std::vector<uint8_t>& rgba, int& w, int& h, std::string& label) {
    if (!OpenClipboard(hwnd)) return false;
    bool ok = false;
    if (HANDLE hd = GetClipboardData(CF_DIB)) {
        auto* bi = (const BITMAPINFO*)GlobalLock(hd);
        if (bi) {
            const BITMAPINFOHEADER& bh = bi->bmiHeader;
            int bpp = bh.biBitCount;
            if ((bpp == 32 || bpp == 24) && (bh.biCompression == BI_RGB || bh.biCompression == BI_BITFIELDS)) {
                w = bh.biWidth; h = std::abs(bh.biHeight);
                bool bottomUp = bh.biHeight > 0;
                size_t colors = bh.biClrUsed ? bh.biClrUsed : 0;
                size_t offset = bh.biSize + colors * sizeof(RGBQUAD) +
                                (bh.biCompression == BI_BITFIELDS ? 3 * sizeof(DWORD) : 0);
                const uint8_t* px = (const uint8_t*)bi + offset;
                size_t stride = ((size_t)w * bpp / 8 + 3) & ~(size_t)3;
                size_t need = offset + stride * h;
                if (GlobalSize(hd) >= need && w > 0 && h > 0) {
                    rgba.resize((size_t)w * h * 4);
                    for (int y = 0; y < h; ++y) {
                        const uint8_t* row = px + stride * (bottomUp ? (h - 1 - y) : y);
                        for (int x = 0; x < w; ++x) {
                            const uint8_t* p = row + (size_t)x * bpp / 8;
                            uint8_t* o = rgba.data() + ((size_t)y * w + x) * 4;
                            o[0] = p[2]; o[1] = p[1]; o[2] = p[0]; o[3] = 255;
                        }
                    }
                    label = "pasted image";
                    ok = true;
                }
            }
            GlobalUnlock(hd);
        }
    }
    if (!ok) {
        if (HANDLE hd = GetClipboardData(CF_HDROP)) {
            wchar_t path[MAX_PATH * 2];
            if (DragQueryFileW((HDROP)hd, 0, path, ARRAYSIZE(path))) {
                std::vector<uint8_t> bytes;
                if (ReadFileBytes(path, bytes)) {
                    int comp = 0;
                    stbi_uc* p = stbi_load_from_memory(bytes.data(), (int)bytes.size(), &w, &h, &comp, 4);
                    if (p) {
                        rgba.assign(p, p + (size_t)w * h * 4);
                        stbi_image_free(p);
                        label = WideToUtf8(fs::path(path).filename().wstring());
                        ok = true;
                    }
                }
            }
        }
    }
    CloseClipboard();
    return ok;
}

} // namespace

// ---------------------------------------------------------------- lifecycle

void App::EmbedInit() {
    embedSettings.Load(SettingsFile());
    embedKey = LoadApiKey(KeyFile(), embedKeyFromEnv);
    strncpy_s(embedModelBuf, sizeof(embedModelBuf), embedSettings.model.c_str(), _TRUNCATE);
}

std::wstring App::EmbedIndexPathForRoot() const { return EmbedIndexPathFor(db.root); }

// ---------------------------------------------------------------- folder exclusions

static std::string NormalizeFolder(std::string f) { return NormalizeEmbedFolder(std::move(f)); }

std::string App::EmbedParentFolder(const AssetEntry& e) {
    size_t slash = e.relPath.rfind('/');
    return slash == std::string::npos ? "" : e.relPath.substr(0, slash);
}

bool App::EmbedExcluded(const AssetEntry& e) const {
    return !embedExcludes.empty() && EmbedFolderListExcludes(embedExcludes, e);
}

bool App::EmbedFolderExcluded(const std::string& relFolder) const {
    std::string f = NormalizeFolder(relFolder);
    return std::find(embedExcludes.begin(), embedExcludes.end(), f) != embedExcludes.end();
}

void App::EmbedSetFolderExcluded(const std::string& relFolder, bool excluded) {
    std::string f = NormalizeFolder(relFolder);
    if (f.empty()) return;
    auto it = std::find(embedExcludes.begin(), embedExcludes.end(), f);
    if (excluded && it == embedExcludes.end()) embedExcludes.push_back(f);
    else if (!excluded && it != embedExcludes.end()) embedExcludes.erase(it);
    else return;
    std::sort(embedExcludes.begin(), embedExcludes.end());
    EmbedSaveExcludes();
    embedCountedAt = -1;
    if (SemanticActive()) { thumbs.FlushPending(); filterDirty = true; }
}

static std::wstring ExcludesFileFor(const std::wstring& indexPath) { return EmbedExcludesFileFor(indexPath); }

void App::EmbedSaveExcludes() const {
    std::error_code ec;
    fs::create_directories(EmbedDir(), ec);
    std::ofstream out(ExcludesFileFor(EmbedIndexPathForRoot()), std::ios::binary | std::ios::trunc);
    out << nlohmann::json(embedExcludes).dump(2);
}

void App::EmbedLoadIndexForRoot() {
    embedExcludes = LoadEmbedExcludes(EmbedIndexPathForRoot());
    embedIndexLoaded = embedIndex.Load(EmbedIndexPathForRoot());
    if (embedIndex.loadedFromBackup)
        SemanticNotify("Index was unreadable; restored from backup.");
    // Query vectors only make sense against the index they were embedded for.
    semanticTextCache.clear();
    SemanticClear();
}

// Workers may be blocked waiting for a sheet render that only this thread can
// do, so never Join() the indexer without aborting those waits first.
void App::EmbedStopIndexing() {
    embedIndexer.Cancel();
    {
        std::lock_guard<std::mutex> lock(sheetMutex);
        sheetAbort = true;
    }
    sheetCv.notify_all();
    embedIndexer.Join();
    std::lock_guard<std::mutex> lock(sheetMutex);
    sheetAbort = false;
    sheetQueue.clear();
}

void App::EmbedShutdown() {
    SafeRelease(semanticImageSRV);
    EmbedStopIndexing();
    embedSheetRT.Release();
    embedIndexer.Join();
    if (semanticJob.thread.joinable()) semanticJob.thread.join();
    if (embedPriceThread.joinable()) embedPriceThread.join();
    if (embedKeyTestThread.joinable()) embedKeyTestThread.join();
    if (embedProbeThread.joinable()) embedProbeThread.join();
}

void App::PumpEmbed() {
    PumpSheetRenders();
    if (embedIndex.Pump() && SemanticActive()) filterDirty = true;
    bool running = embedIndexer.Running();
    if (!running && embedIndexer.HasThreads()) {
        embedIndexer.Join();
        if (SemanticActive()) filterDirty = true;
    }
    if (embedIndexerWasRunning && !running) {
        char msg[160];
        if (embedIndexer.aborted)
            snprintf(msg, sizeof(msg), "Indexing stopped: %s", embedIndexer.abortReason.c_str());
        else if (embedIndexer.budgetHit)
            snprintf(msg, sizeof(msg), "Spending cap reached: %d indexed, $%.4f. Run again to continue.",
                     embedIndexer.done.load(), embedIndexer.costSoFar());
        else
            snprintf(msg, sizeof(msg), "Indexed %d assets, $%.4f.", embedIndexer.done.load(), embedIndexer.costSoFar());
        SemanticNotify(msg);
        embedCountedAt = -1; // refresh counts now
    }
    embedIndexerWasRunning = running;
    // Counting walks every asset; refresh about once a second, not per frame.
    double now = ImGui::GetTime();
    if (now - embedCountedAt > 1.0 || embedCountedEntries != db.entries.size()) {
        embedCountedAt = now;
        embedCountedEntries = db.entries.size();
        embedIndexableCount = 0;
        for (const AssetEntry& e : db.entries) if (EmbedIsIndexable(e)) ++embedIndexableCount;
        embedUnindexedTex = embedUnindexedAudio = embedUnindexedModel = embedAudioUnknown = 0;
        embedAudioSecondsKnown = 0;
        embedIndexedByType[0] = embedIndexedByType[1] = embedIndexedByType[2] = 0;
        if (embedIndexLoaded)
            for (const AssetEntry& e : db.entries)
                if (!EmbedExcluded(e) && embedIndex.HasCurrent(e)) ++embedIndexedByType[(int)e.type];
        embedPackCoverage.clear();
        if (embedIndexLoaded)
            for (const AssetEntry& e : db.entries) {
                if (!EmbedIsIndexable(e) || EmbedExcluded(e)) continue;
                auto& cov = embedPackCoverage[e.pack];
                ++cov.second;
                if (embedIndex.HasCurrent(e)) ++cov.first;
            }
        for (int i : EmbedUnindexed()) {
            const AssetEntry& e = db.entries[i];
            if (e.type == AssetType::Model) { ++embedUnindexedModel; continue; }
            if (e.type != AssetType::Audio) { ++embedUnindexedTex; continue; }
            ++embedUnindexedAudio;
            auto it = audioDurations.find(e.relPath);
            if (it == audioDurations.end()) ++embedAudioUnknown;
            else embedAudioSecondsKnown += (std::min)(it->second, (double)embedSettings.audioSeconds);
        }
        embedUnindexedCount = embedUnindexedTex + embedUnindexedAudio + embedUnindexedModel;
        embedStaleCount = 0;
        if (embedIndexLoaded)
            for (const AssetEntry& e : db.entries)
                if (!EmbedExcluded(e) && embedIndex.IsStale(e)) ++embedStaleCount;
        embedMissingCount = (embedIndexLoaded && !db.scanning) ? embedIndex.CountMissing(db.entries) : 0;
    }
    if (embedWindowOpen) EmbedProbeDurations();
    // Vectors for files that are no longer in the library are never removed
    // automatically; the window reports them and offers a button.
    // async query result
    {
        std::lock_guard<std::mutex> lock(semanticJob.m);
        if (semanticJob.ready) {
            semanticJob.ready = false;
            if (semanticJob.thread.joinable()) semanticJob.thread.join();
            semanticBusy = false;
            if (semanticJob.id == semanticJobId) {
                if (semanticJob.result.ok) {
                    if (semanticJob.kind == SemanticKind::Text) { SafeRelease(semanticImageSRV); semanticAssetIndex = -1; }
                    semanticQuery = semanticJob.result.vec;
                    semanticKind = semanticJob.kind;
                    semanticLabel = semanticJob.label;
                    semanticError.clear();
                    if (semanticKind == SemanticKind::Text) semanticTextCache[semanticLabel] = semanticQuery;
                    semanticSearch = true;
                } else {
                    semanticError = semanticJob.result.error;
                }
                thumbs.FlushPending();
                filterDirty = true;
            }
        }
    }
    {
        std::lock_guard<std::mutex> lock(embedPriceMutex);
        if (embedPriceReady) { embedPriceReady = false; embedPriceFetching = false; embedPriceFetched = true; embedPriceThread.join(); }
    }
    {
        std::lock_guard<std::mutex> lock(embedKeyTestMutex);
        if (embedKeyTestReady) { embedKeyTestReady = false; embedKeyTesting = false; embedKeyTestThread.join(); }
    }
}

// ---------------------------------------------------------------- indexing

std::vector<int> App::EmbedUnindexed(int types) const {
    std::vector<int> out;
    bool usable = embedIndexLoaded && embedIndex.Matches(embedSettings);
    for (int i = 0; i < (int)db.entries.size(); ++i) {
        const AssetEntry& e = db.entries[i];
        if (!EmbedIsIndexable(e) || EmbedExcluded(e)) continue;
        int bit = e.type == AssetType::Audio ? EmbedAudio : e.type == AssetType::Model ? EmbedModel : EmbedTex;
        if (!(types & bit)) continue;
        if (usable && embedIndex.HasCurrent(e)) continue;
        out.push_back(i);
    }
    return out;
}

// Reads container headers of un-indexed sounds in the background so the cost
// estimate can be exact (25 tokens per second) instead of an upper bound.
void App::EmbedProbeDurations() {
    {
        std::lock_guard<std::mutex> lock(embedProbeMutex);
        for (auto& [rel, sec] : embedProbeResults) audioDurations[rel] = sec;
        if (!embedProbeResults.empty()) { embedProbeResults.clear(); embedCountedAt = -1; }
        if (embedProbeDone) { embedProbeDone = false; embedProbing = false; embedProbeThread.join(); }
    }
    if (embedProbing || db.scanning) return;
    std::vector<std::pair<std::string, std::wstring>> todo;
    for (int i : EmbedUnindexed(EmbedAudio)) {
        const AssetEntry& e = db.entries[i];
        if (!audioDurations.count(e.relPath)) todo.emplace_back(e.relPath, e.path);
        if (todo.size() >= 256) break; // batch; PumpEmbed starts the next one
    }
    if (todo.empty()) return;
    embedProbing = true;
    embedProbeThread = std::thread([this, todo = std::move(todo)] {
        for (auto& [rel, path] : todo) {
            double sec = AudioDurationSeconds(path);
            std::lock_guard<std::mutex> lock(embedProbeMutex);
            embedProbeResults.emplace_back(rel, sec);
        }
        std::lock_guard<std::mutex> lock(embedProbeMutex);
        embedProbeDone = true;
    });
}

void App::EmbedKeepStaleVectors() {
    if (!embedIndexLoaded || embedIndexer.Running()) return;
    int n = 0;
    for (const AssetEntry& e : db.entries)
        if (!EmbedExcluded(e) && embedIndex.IsStale(e)) { embedIndex.Adopt(e); ++n; }
    embedCountedAt = -1;
    if (SemanticActive()) { thumbs.FlushPending(); filterDirty = true; }
    SemanticNotify("Kept vectors for " + std::to_string(n) + " changed files.");
}

std::vector<int> App::EmbedPendingIn(const std::vector<int>& candidates) const {
    std::vector<int> out;
    bool usable = embedIndexLoaded && embedIndex.Matches(embedSettings);
    for (int i : candidates) {
        if (i < 0 || i >= (int)db.entries.size()) continue;
        const AssetEntry& e = db.entries[i];
        if (!EmbedIsIndexable(e) || EmbedExcluded(e)) continue;
        if (usable && embedIndex.HasCurrent(e)) continue;
        out.push_back(i);
    }
    return out;
}

App::EmbedEstimate App::EmbedEstimateFor(const std::vector<int>& candidates) const {
    EmbedEstimate est;
    for (int i : EmbedPendingIn(candidates)) {
        const AssetEntry& e = db.entries[i];
        ++est.count;
        if (e.type == AssetType::Audio) {
            auto it = audioDurations.find(e.relPath);
            double win = embedSettings.audioSeconds;
            double sec = it == audioDurations.end() ? win : (std::min)(it->second, win);
            if (it == audioDurations.end()) est.upperBound = true;
            est.cost += sec * kEmbedTokensPerAudioSecond * embedPrices.audio;
        } else {
            est.cost += kEmbedTokensPerImage * embedPrices.image;
        }
    }
    return est;
}

std::vector<int> App::EmbedEntriesUnderFolder(const std::string& relFolder) const {
    std::vector<int> out;
    std::string f = ToLowerAscii(relFolder);
    for (int i = 0; i < (int)db.entries.size(); ++i) {
        const std::string& p = db.entries[i].relPathLower;
        if (p.size() > f.size() && p[f.size()] == '/' && p.compare(0, f.size(), f) == 0) out.push_back(i);
    }
    return out;
}

bool App::EmbedStartIndexingSubset(std::vector<int> candidates, const std::string& what) {
    if (embedIndexer.Running()) { SemanticNotify("Already indexing."); return false; }
    if (embedKey.empty()) { SemanticNotify("No API key. Semantic > Settings & indexing"); return false; }
    if (db.scanning) { SemanticNotify("Scan still running."); return false; }
    if (embedIndexLoaded && !embedIndex.Empty() && !embedIndex.Matches(embedSettings)) {
        SemanticNotify("Settings changed; rebuild the index first.");
        return false;
    }
    std::vector<int> pending = EmbedPendingIn(candidates);
    if (pending.empty()) { SemanticNotify("Nothing to index in " + what + "."); return false; }
    embedIndexer.Join();
    if (!embedIndexLoaded) {
        embedIndex.Reset(embedSettings.model, embedSettings.mode, embedSettings.dims, EmbedIndexPathForRoot());
        embedIndexLoaded = true;
    }
    std::vector<EmbedJob> jobs;
    for (int i : pending) {
        const AssetEntry& e = db.entries[i];
        jobs.push_back({ i, e.type, e.relPath, e.name, e.path, e.mtime, e.size });
    }
    EmbedClient client{ embedKey, embedSettings.model, embedSettings.dims };
    Thumbs* th = &thumbs;
    double audioWin = embedSettings.audioSeconds;
    bool audioTrim = embedSettings.audioTrimSilence;
    embedIndexer.Start(&embedIndex, client, embedSettings.mode, embedSettings.concurrency, std::move(jobs),
        [this, th, audioWin, audioTrim](const EmbedJob& j, std::vector<uint8_t>& data, const char*& mime) {
            if (j.type == AssetType::Model) {
                std::vector<uint8_t> rgba;
                mime = "image/jpeg";
                return RenderSheetRequest(j, rgba) &&
                       EncodeJpeg(rgba.data(), kEmbedModelView * kEmbedModelSheetCols,
                                  kEmbedModelView * kEmbedModelSheetRows, data);
            }
            if (j.type == AssetType::Audio) {
                PcmData pcm = DecodeAudioFile(j.path);
                mime = "audio/wav";
                return EncodeWav16kMono(pcm, 0, pcm.frames, data, audioWin, audioTrim) > 0;
            }
            int w = 0, h = 0;
            std::vector<uint8_t> rgba;
            mime = "image/jpeg";
            return th->LoadOrMakeTextureThumb(j.path, j.mtime, j.size, w, h, rgba) && EncodeJpeg(rgba.data(), w, h, data);
        }, embedSettings.budgetUsd);
    SemanticNotify("Indexing " + std::to_string(pending.size()) + " in " + what + ".");
    return true;
}

// "Index 42 assets in <what> (about $0.0049)" as a menu item; disabled with a reason when it can't run.
void App::EmbedIndexMenuItem(const std::vector<int>& candidates, const char* what) {
    EmbedEstimate est = EmbedEstimateFor(candidates);
    char label[160];
    if (est.count == 0)
        snprintf(label, sizeof(label), "Nothing to index in %s", what);
    else if (embedPrices.image > 0)
        snprintf(label, sizeof(label), "Index %d asset%s in %s (%s$%.4f)", est.count, est.count == 1 ? "" : "s", what,
                 est.upperBound ? "up to " : "about ", est.cost);
    else
        snprintf(label, sizeof(label), "Index %d asset%s in %s", est.count, est.count == 1 ? "" : "s", what);
    bool can = est.count > 0 && !embedIndexer.Running() && !db.scanning;
    ImGui::BeginDisabled(!can);
    if (ImGui::MenuItem(label)) EmbedStartIndexingSubset(candidates, what);
    ImGui::EndDisabled();
    if (embedIndexer.Running() && ImGui::IsItemHovered(ImGuiHoveredFlags_AllowWhenDisabled))
        ImGui::SetTooltip("Already indexing");
}

void App::EmbedStartIndexing(bool rebuild, int types) {
    if (embedIndexer.Running() || embedKey.empty() || db.root.empty()) return;
    embedIndexer.Join();
    // Resetting the index drops every type's vectors, so a reset always re-queues all types.
    if (rebuild || (embedIndexLoaded && !embedIndex.Matches(embedSettings))) types = EmbedAll;
    if (rebuild || !embedIndexLoaded || !embedIndex.Matches(embedSettings)) {
        embedIndex.Reset(embedSettings.model, embedSettings.mode, embedSettings.dims, EmbedIndexPathForRoot());
        embedIndexLoaded = true;
        semanticTextCache.clear();
        SemanticClear();
    }
    std::vector<EmbedJob> jobs;
    for (int i : EmbedUnindexed(types)) {
        const AssetEntry& e = db.entries[i];
        jobs.push_back({ i, e.type, e.relPath, e.name, e.path, e.mtime, e.size });
    }
    if (jobs.empty()) return;
    EmbedClient client{ embedKey, embedSettings.model, embedSettings.dims };
    Thumbs* th = &thumbs;
    double audioWin = embedSettings.audioSeconds;
    bool audioTrim = embedSettings.audioTrimSilence;
    embedIndexer.Start(&embedIndex, client, embedSettings.mode, embedSettings.concurrency, std::move(jobs),
        [this, th, audioWin, audioTrim](const EmbedJob& j, std::vector<uint8_t>& data, const char*& mime) {
            if (j.type == AssetType::Model) {
                std::vector<uint8_t> rgba;
                mime = "image/jpeg";
                return RenderSheetRequest(j, rgba) &&
                       EncodeJpeg(rgba.data(), kEmbedModelView * kEmbedModelSheetCols,
                                  kEmbedModelView * kEmbedModelSheetRows, data);
            }
            if (j.type == AssetType::Audio) {
                PcmData pcm = DecodeAudioFile(j.path);
                mime = "audio/wav";
                return EncodeWav16kMono(pcm, 0, pcm.frames, data, audioWin, audioTrim) > 0;
            }
            int w = 0, h = 0;
            std::vector<uint8_t> rgba;
            mime = "image/jpeg";
            return th->LoadOrMakeTextureThumb(j.path, j.mtime, j.size, w, h, rgba) && EncodeJpeg(rgba.data(), w, h, data);
        }, embedSettings.budgetUsd);
}

// ---------------------------------------------------------------- model sheets

// Worker thread: parse the model here (CPU), then hand it to the main thread
// for the GPU render and wait for the pixels.
bool App::RenderSheetRequest(const EmbedJob& job, std::vector<uint8_t>& rgba) {
    bool isGltf = job.relPath.size() > 4 && (job.relPath.ends_with(".glb") || job.relPath.ends_with(".gltf"));
    SheetRequest req;
    req.model = isGltf ? LoadGlbCpu(job.path) : LoadFbxObjCpu(job.path);
    if (!req.model.valid) return false;
    std::unique_lock<std::mutex> lock(sheetMutex);
    if (sheetAbort) return false;
    sheetQueue.push_back(&req);
    sheetCv.wait(lock, [&] { return req.done || sheetAbort; });
    if (!req.done) {
        // aborted while queued: make sure nobody renders a dead request
        auto it = std::find(sheetQueue.begin(), sheetQueue.end(), &req);
        if (it != sheetQueue.end()) sheetQueue.erase(it);
        return false;
    }
    rgba = std::move(req.rgba);
    return req.ok;
}

bool RenderModelSheet(Gfx& gfx, ModelRenderer& renderer, OffscreenTarget& rt, const CpuModel& model,
                      std::vector<uint8_t>& rgba) {
    const int view = kEmbedModelView, cols = kEmbedModelSheetCols, rows = kEmbedModelSheetRows;
    const int sheetW = view * cols, sheetH = view * rows;
    if (rt.w != view || rt.h != view) return false;
    GpuModel gpu;
    if (!gpu.Create(gfx, model)) return false;
    rgba.assign((size_t)sheetW * sheetH * 4, 0);
    // yaw/pitch per cell, row-major: hero 3/4, front, back, left, right, top
    const float views[6][2] = { { 35, -22 }, { 0, -8 }, { 180, -8 }, { 90, -8 }, { -90, -8 }, { 0, -80 } };
    const float grey[4] = { 0.216f, 0.216f, 0.216f, 1.0f }; // ~#808080 after sRGB encode
    bool ok = true;
    std::vector<uint8_t> px;
    for (int i = 0; i < cols * rows && ok; ++i) {
        OrbitCamera cam;
        cam.yawDeg = views[i][0];
        cam.pitchDeg = views[i][1];
        cam.FrameModel(gpu.bmin, gpu.bmax);
        renderer.Render(gfx, gpu, rt, cam, grey);
        if (!rt.ReadPixels(gfx.ctx, px) || px.size() < (size_t)view * view * 4) { ok = false; break; }
        int cx = (i % cols) * view, cy = (i / cols) * view;
        for (int y = 0; y < view; ++y)
            memcpy(rgba.data() + ((size_t)(cy + y) * sheetW + cx) * 4, px.data() + (size_t)y * view * 4, (size_t)view * 4);
    }
    gpu.Release();
    return ok;
}

// Main thread: render up to a few queued models per frame.
void App::PumpSheetRenders() {
    for (int n = 0; n < 3; ++n) {
        SheetRequest* req = nullptr;
        {
            std::lock_guard<std::mutex> lock(sheetMutex);
            if (sheetQueue.empty()) return;
            req = sheetQueue.front();
            sheetQueue.pop_front();
        }
        if (!embedSheetRT.tex) embedSheetRT.Create(gfx->device, kEmbedModelView, kEmbedModelView, /*withStaging*/ true);
        bool ok = embedSheetRT.tex && RenderModelSheet(*gfx, renderer, embedSheetRT, req->model, req->rgba);
        {
            std::lock_guard<std::mutex> lock(sheetMutex);
            req->ok = ok;
            req->done = true;
        }
        sheetCv.notify_all();
    }
}

// ---------------------------------------------------------------- queries

void App::SemanticClear() {
    semanticKind = SemanticKind::None;
    semanticQuery.clear();
    semanticLabel.clear();
    semanticScore.clear();
    semanticError.clear();
    semanticAssetIndex = -1;
    SafeRelease(semanticImageSRV);
    semanticBusy = false;
    ++semanticJobId; // drop any in-flight result
    filterDirty = true;
}

void App::SemanticNotify(const std::string& text) {
    semanticNotice = text;
    semanticNoticeUntil = ImGui::GetTime() + 6.0;
}

void App::SemanticQueryText(const std::string& text) {
    if (text.empty() || !SemanticReady()) return;
    auto cached = semanticTextCache.find(text);
    if (cached != semanticTextCache.end()) {
        SafeRelease(semanticImageSRV);
        semanticAssetIndex = -1;
        semanticQuery = cached->second;
        semanticKind = SemanticKind::Text;
        semanticLabel = text;
        semanticError.clear();
        thumbs.FlushPending();
        filterDirty = true;
        return;
    }
    if (semanticJob.thread.joinable()) semanticJob.thread.join();
    uint64_t id = ++semanticJobId;
    EmbedClient client{ embedKey, embedIndex.model, embedIndex.dims };
    semanticBusy = true;
    semanticError.clear();
    semanticJob.thread = std::thread([this, id, client, text] {
        EmbedResult r = client.EmbedText(text);
        std::lock_guard<std::mutex> lock(semanticJob.m);
        semanticJob.result = std::move(r);
        semanticJob.id = id;
        semanticJob.kind = SemanticKind::Text;
        semanticJob.label = text;
        semanticJob.ready = true;
    });
}

void App::SemanticQueryImage(const uint8_t* rgba, int w, int h, const std::string& label) {
    if (!SemanticReady()) return;
    std::vector<uint8_t> fitted, jpeg;
    int fw = 0, fh = 0;
    FitRgba(rgba, w, h, Thumbs::kThumbSize, fitted, fw, fh);
    if (!EncodeJpeg(fitted.data(), fw, fh, jpeg)) { semanticError = "could not encode query image"; return; }
    SafeRelease(semanticImageSRV);
    semanticImageSRV = gfx->CreateTextureSRV(fitted.data(), fw, fh, false);
    semanticImageW = fw; semanticImageH = fh;
    semanticAssetIndex = -1;
    semanticSearch = true;
    semanticError.clear();
    if (semanticJob.thread.joinable()) semanticJob.thread.join();
    uint64_t id = ++semanticJobId;
    EmbedClient client{ embedKey, embedIndex.model, embedIndex.dims };
    semanticBusy = true;
    semanticJob.thread = std::thread([this, id, client, jpeg = std::move(jpeg), label] {
        EmbedResult r = client.EmbedMedia(jpeg, "image/jpeg", "");
        std::lock_guard<std::mutex> lock(semanticJob.m);
        semanticJob.result = std::move(r);
        semanticJob.id = id;
        semanticJob.kind = SemanticKind::Image;
        semanticJob.label = label;
        semanticJob.ready = true;
    });
}

void App::SemanticQueryAudio(const PcmData& pcm, uint64_t f0, uint64_t f1, const std::string& label) {
    if (!SemanticReady()) return;
    std::vector<uint8_t> wav;
    double sec = EncodeWav16kMono(pcm, f0, f1, wav, embedSettings.audioSeconds, embedSettings.audioTrimSilence);
    if (sec <= 0) { SemanticNotify("Could not decode that audio."); return; }
    // waveform of exactly what is sent, as the query chip
    uint64_t w0 = f0, w1 = f1;
    EmbedAudioWindow(pcm, w0, w1, embedSettings.audioSeconds, embedSettings.audioTrimSilence);
    PcmData part;
    part.channels = pcm.channels; part.sampleRate = pcm.sampleRate; part.valid = true;
    part.frames = w1 - w0;
    part.samples.assign(pcm.samples.begin() + (size_t)w0 * pcm.channels, pcm.samples.begin() + (size_t)w1 * pcm.channels);
    std::vector<uint8_t> wave = RenderWaveformThumb(part, 256, 96);
    SafeRelease(semanticImageSRV);
    semanticImageSRV = gfx->CreateTextureSRV(wave.data(), 256, 96, false);
    semanticImageW = 256; semanticImageH = 96;
    semanticAssetIndex = -1;
    semanticSearch = true;
    semanticError.clear();
    if (semanticJob.thread.joinable()) semanticJob.thread.join();
    uint64_t id = ++semanticJobId;
    EmbedClient client{ embedKey, embedIndex.model, embedIndex.dims };
    semanticBusy = true;
    char lbl[256];
    snprintf(lbl, sizeof(lbl), "%s (%.1f s)", label.c_str(), sec);
    std::string labelSec = lbl;
    semanticJob.thread = std::thread([this, id, client, wav = std::move(wav), labelSec] {
        EmbedResult r = client.EmbedMedia(wav, "audio/wav", "");
        std::lock_guard<std::mutex> lock(semanticJob.m);
        semanticJob.result = std::move(r);
        semanticJob.id = id;
        semanticJob.kind = SemanticKind::Audio;
        semanticJob.label = labelSec;
        semanticJob.ready = true;
    });
}

void App::SemanticQueryPreviewSelection() {
    if (previewIndex < 0 || previewIndex >= (int)db.entries.size() || !previewPcm.valid) return;
    const AssetEntry& e = db.entries[previewIndex];
    if (e.type != AssetType::Audio) return;
    uint64_t f0 = 0, f1 = 0;
    bool hasSel = AudioSelectionFrames(f0, f1);
    SemanticQueryAudio(previewPcm, f0, f1, hasSel ? "selection of " + e.name : e.name);
}

bool App::SemanticFindSimilar(int index) {
    if (index < 0 || index >= (int)db.entries.size()) return false;
    const AssetEntry& e = db.entries[index];
    if (!SemanticReady()) { SemanticNotify("No index yet. Semantic > Settings & indexing"); return false; }
    if (EmbedExcluded(e)) { SemanticNotify(e.name + " is excluded."); return false; }
    if (!embedIndex.VectorFor(e)) {
        if (e.type == AssetType::Model && !EmbedModelRenderable(e.ext))
            SemanticNotify(e.name + ": format cannot be rendered.");
        else if (!EmbedIsIndexable(e))
            SemanticNotify(e.name + " is an auxiliary map.");
        else
            SemanticNotify(e.name + " is not indexed.");
        return false;
    }
    SemanticQueryAsset(index);
    return true;
}

void App::SemanticQueryAsset(int index) {
    if (index < 0 || index >= (int)db.entries.size() || !embedIndexLoaded) return;
    const AssetEntry& e = db.entries[index];
    const float* v = embedIndex.VectorFor(e);
    if (!v) { semanticError = e.name + " is not indexed"; return; }
    ++semanticJobId;
    SafeRelease(semanticImageSRV);
    semanticAssetIndex = index;
    semanticBusy = false;
    semanticQuery.assign(v, v + embedIndex.dims);
    semanticKind = SemanticKind::Asset;
    semanticLabel = "similar to " + e.name;
    semanticError.clear();
    semanticSearch = true;
    thumbs.FlushPending();
    filterDirty = true;
}

bool App::PasteImageQuery() {
    // A copied file (Explorer) may be an image or a sound; a bitmap is an image.
    if (IsClipboardFormatAvailable(CF_HDROP) && OpenClipboard(hwnd)) {
        std::wstring file;
        if (HANDLE hd = GetClipboardData(CF_HDROP)) {
            wchar_t path[MAX_PATH * 2];
            if (DragQueryFileW((HDROP)hd, 0, path, ARRAYSIZE(path))) file = path;
        }
        CloseClipboard();
        if (!file.empty()) return DropFileQuery(file);
    }
    std::vector<uint8_t> rgba;
    int w = 0, h = 0;
    std::string label;
    if (!ReadClipboardImage(hwnd, rgba, w, h, label)) {
        SemanticNotify("Clipboard has no image or file.");
        return false;
    }
    SemanticQueryImage(rgba.data(), w, h, label);
    return true;
}

bool App::DropFileQuery(const std::wstring& file) {
    std::string name = WideToUtf8(fs::path(file).filename().wstring());
    std::string ext = ToLowerAscii(WideToUtf8(fs::path(file).extension().wstring()));
    if (ext == ".wav" || ext == ".ogg" || ext == ".mp3" || ext == ".flac") {
        PcmData pcm = DecodeAudioFile(file);
        if (!pcm.valid) { SemanticNotify("Could not decode " + name + "."); return false; }
        SemanticQueryAudio(pcm, 0, pcm.frames, name);
        return true;
    }
    std::vector<uint8_t> bytes;
    if (!ReadFileBytes(file, bytes)) { SemanticNotify("Could not read the dropped file."); return false; }
    int w = 0, h = 0, comp = 0;
    stbi_uc* p = stbi_load_from_memory(bytes.data(), (int)bytes.size(), &w, &h, &comp, 4);
    if (!p) { SemanticNotify("Dropped file is not an image or sound."); return false; }
    std::vector<uint8_t> rgba(p, p + (size_t)w * h * 4);
    stbi_image_free(p);
    SemanticQueryImage(rgba.data(), w, h, WideToUtf8(fs::path(file).filename().wstring()));
    return true;
}

// ---------------------------------------------------------------- window

void App::DrawEmbedWindow() {
    ImGui::SetNextWindowSize(ImVec2(520, 560), ImGuiCond_FirstUseEver);
    if (!ImGui::Begin("Semantic indexing", &embedWindowOpen)) { ImGui::End(); return; }

    bool running = embedIndexer.Running();

    // ---- status header
    {
        const char* state; ImVec4 col = Theme::Accent();
        if (embedKey.empty()) { state = "No API key"; col = ImGui::GetStyleColorVec4(ImGuiCol_TextDisabled); }
        else if (db.root.empty()) { state = "No asset folder"; col = ImGui::GetStyleColorVec4(ImGuiCol_TextDisabled); }
        else if (running) state = "Indexing...";
        else if (!embedIndexLoaded || embedIndex.Empty()) state = "Ready to index";
        else if (embedUnindexedCount > 0) state = "Ready; some assets not indexed";
        else state = "Fully indexed";
        ImGui::TextColored(col, "%s", state);
        ImGui::Spacing();
    }

    // ---- API key
    ImGui::SeparatorText("OpenRouter API key");
    if (embedKey.empty())
        ImGui::TextDisabled("Paste a key, or set OPENROUTER_API_KEY.");
    else if (embedKeyFromEnv)
        ImGui::TextDisabled("From OPENROUTER_API_KEY.");
    else
        ImGui::TextDisabled("Stored, encrypted for this user.");
    ImGui::SetNextItemWidth(-120);
    ImGui::InputTextWithHint("##key", "sk-or-...", embedKeyBuf, sizeof(embedKeyBuf), ImGuiInputTextFlags_Password);
    ImGui::SameLine();
    ImGui::BeginDisabled(!embedKeyBuf[0] || running);
    if (ImGui::Button("Save key", ImVec2(-1, 0))) {
        std::string key = embedKeyBuf;
        while (!key.empty() && (key.back() == ' ' || key.back() == '\n' || key.back() == '\r')) key.pop_back();
        if (SaveApiKey(KeyFile(), key)) { embedKey = key; embedKeyFromEnv = false; embedKeyBuf[0] = 0; embedPriceFetched = false; }
    }
    ImGui::EndDisabled();
    ImGui::BeginDisabled(embedKey.empty() || embedKeyTesting);
    if (ImGui::Button(embedKeyTesting ? "Testing..." : "Test key")) {
        if (embedKeyTestThread.joinable()) embedKeyTestThread.join();
        embedKeyTesting = true;
        std::string key = embedKey;
        embedKeyTestThread = std::thread([this, key] {
            std::string err;
            bool ok = EmbedClient::ValidateKey(key, err);
            std::lock_guard<std::mutex> lock(embedKeyTestMutex);
            embedKeyTestResult = ok ? "Key is valid." : "Key rejected: " + err;
            embedKeyTestReady = true;
        });
    }
    ImGui::EndDisabled();
    ImGui::SameLine();
    ImGui::BeginDisabled(embedKey.empty() || embedKeyFromEnv || running);
    if (ImGui::Button("Remove stored key")) {
        SaveApiKey(KeyFile(), "");
        embedKey = LoadApiKey(KeyFile(), embedKeyFromEnv);
        embedKeyTestResult.clear();
    }
    ImGui::EndDisabled();
    if (!embedKeyTestResult.empty()) { ImGui::SameLine(); ImGui::TextDisabled("%s", embedKeyTestResult.c_str()); }

    // ---- settings
    ImGui::SeparatorText("Model");
    ImGui::BeginDisabled(running);
    bool changed = false;
    ImGui::SetNextItemWidth(-100);
    if (ImGui::InputText("Model id", embedModelBuf, sizeof(embedModelBuf))) {
        embedSettings.model = embedModelBuf;
        changed = true;
    }
    ImGui::SetNextItemWidth(180);
    int mode = (int)embedSettings.mode;
    static const char* modeNames[] = { "Hybrid (media + names)", "Media only" };
    if (ImGui::Combo("Input", &mode, modeNames, 2)) { embedSettings.mode = (EmbedMode)mode; changed = true; }
    ImGui::SameLine();
    ImGui::TextDisabled("(?)");
    if (ImGui::IsItemHovered())
        ImGui::SetTooltip("Hybrid adds file and folder names: better text search.\nMedia only: purer search-by-example.");
    ImGui::SetNextItemWidth(180);
    int dimsIdx = embedSettings.dims == 3072 ? 1 : 0;
    static const char* dimNames[] = { "768 (compact)", "3072 (full)" };
    if (ImGui::Combo("Dimensions", &dimsIdx, dimNames, 2)) { embedSettings.dims = dimsIdx ? 3072 : 768; changed = true; }
    ImGui::SetNextItemWidth(180);
    if (ImGui::SliderInt("Parallel requests", &embedSettings.concurrency, 1, 16)) changed = true;
    ImGui::SetNextItemWidth(180);
    float budget = (float)embedSettings.budgetUsd;
    if (ImGui::InputFloat("Spending cap per run ($)", &budget, 0.5f, 5.0f, "%.2f")) {
        embedSettings.budgetUsd = (std::max)(0.0, (double)budget);
        changed = true;
    }
    ImGui::SameLine();
    ImGui::TextDisabled("(?)");
    if (ImGui::IsItemHovered())
        ImGui::SetTooltip("0 = no cap. Stops the run at this cost; progress is kept.");
    ImGui::SetNextItemWidth(180);
    if (ImGui::SliderInt("Audio window (s)", &embedSettings.audioSeconds, 4, 30)) changed = true;
    ImGui::SameLine();
    ImGui::TextDisabled("(?)");
    if (ImGui::IsItemHovered())
        ImGui::SetTooltip("Seconds of each clip sent. Cost is per second; 10 s matched full clips 10/12 in testing.");
    if (ImGui::Checkbox("Trim silence", &embedSettings.audioTrimSilence)) changed = true;
    ImGui::SameLine();
    ImGui::TextDisabled("(?)");
    if (ImGui::IsItemHovered())
        ImGui::SetTooltip("Skip leading and trailing silence (below -50 dBFS) before the window.");
    ImGui::EndDisabled();
    if (changed) embedSettings.Save(SettingsFile());
    if (embedPriceModel != embedSettings.model) embedPriceFetched = false;

    // price lookup (once per key/model)
    if (!embedKey.empty() && !embedPriceFetched && !embedPriceFetching) {
        if (embedPriceThread.joinable()) embedPriceThread.join();
        embedPriceFetching = true;
        embedPriceModel = embedSettings.model;
        std::string key = embedKey, model = embedSettings.model;
        embedPriceThread = std::thread([this, key, model] {
            EmbedClient::Prices prices; std::string err;
            EmbedClient::FetchPrices(key, model, prices, err);
            std::lock_guard<std::mutex> lock(embedPriceMutex);
            embedPrices = prices;
            embedPriceError = err;
            embedPriceReady = true;
        });
    }

    // ---- index
    ImGui::SeparatorText("Index");
    if (db.root.empty()) {
        ImGui::TextDisabled("Choose an asset folder first.");
        ImGui::End();
        return;
    }
    int todo = embedUnindexedCount;
    bool mismatch = embedIndexLoaded && !embedIndex.Empty() && !embedIndex.Matches(embedSettings);

    if (embedIndexLoaded && !embedIndex.Empty()) {
        ImGui::Text("Index: %d textures, %d models, %d sounds", embedIndexedByType[0], embedIndexedByType[1], embedIndexedByType[2]);
        ImGui::SameLine();
        ImGui::TextDisabled("(%s, %s, %d dims)", embedIndex.model.c_str(),
                            embedIndex.mode == EmbedMode::Hybrid ? "hybrid" : "media only", embedIndex.dims);
    } else {
        ImGui::TextDisabled("No index for this library yet.");
    }
    if (mismatch)
        ImGui::TextColored(Theme::Accent(), "Settings differ from the index; rebuild required.");

    bool priceKnown = embedPrices.image > 0 || embedPrices.audio > 0;
    auto priceNote = [&]() -> std::string {
        if (priceKnown) return "";
        if (embedPriceFetching) return " (fetching price...)";
        return embedPriceError.empty() ? " (price unknown)" : " (price unknown: " + embedPriceError + ")";
    };
    bool canIndex = !embedKey.empty() && !db.scanning && !running;
    // A settings mismatch means the next run must reset the index; only the
    // all-types rebuild button may do that, or the other types would vanish.
    bool canIndexType = canIndex && !mismatch;
    auto mismatchTip = [&] {
        if (mismatch && ImGui::IsItemHovered(ImGuiHoveredFlags_AllowWhenDisabled))
            ImGui::SetTooltip("Rebuild required");
    };

    // -- per-type rows: what is left to index, what it costs, and a button each
    double texCost = embedUnindexedTex * (double)kEmbedTokensPerImage * embedPrices.image;
    double modelCost = embedUnindexedModel * (double)kEmbedTokensPerImage * embedPrices.image;
    double audioSeconds = embedAudioSecondsKnown + embedAudioUnknown * embedSettings.audioSeconds; // unknown = upper bound
    double audioCost = audioSeconds * kEmbedTokensPerAudioSecond * embedPrices.audio;
    char audioLen[64];
    if (audioSeconds >= 3600) snprintf(audioLen, sizeof(audioLen), "%.1f h", audioSeconds / 3600);
    else if (audioSeconds >= 60) snprintf(audioLen, sizeof(audioLen), "%.1f min", audioSeconds / 60);
    else snprintf(audioLen, sizeof(audioLen), "%.0f s", audioSeconds);

    struct Row { const char* name; int count; double cost; bool upperBound; int type; const char* tip; };
    char audioNote[96];
    snprintf(audioNote, sizeof(audioNote), "%s%s", audioLen, embedAudioUnknown ? " (measuring)" : "");
    const Row rows[] = {
        { "Textures", embedUnindexedTex, texCost, false, EmbedTex,
          "256 px thumbnail; aux maps skipped" },
        { "Models", embedUnindexedModel, modelCost, false, EmbedModel,
          "Six rendered views; DAE skipped" },
        { "Sounds", embedUnindexedAudio, audioCost, embedAudioUnknown > 0, EmbedAudio,
          "16 kHz mono; ~14x texture cost per second" },
    };
    if (ImGui::BeginTable("embedtypes", 3, ImGuiTableFlags_SizingStretchProp)) {
        ImGui::TableSetupColumn("type", ImGuiTableColumnFlags_WidthFixed, 70.0f);
        ImGui::TableSetupColumn("status", ImGuiTableColumnFlags_WidthStretch);
        ImGui::TableSetupColumn("action", ImGuiTableColumnFlags_WidthFixed, 150.0f);
        for (const Row& r : rows) {
            ImGui::TableNextRow();
            ImGui::TableNextColumn();
            ImGui::AlignTextToFramePadding();
            ImGui::TextUnformatted(r.name);
            ImGui::TableNextColumn();
            ImGui::AlignTextToFramePadding();
            if (r.count > 0) {
                std::string line = std::to_string(r.count) + " to index";
                if (r.type == EmbedAudio) line += std::string(", ") + audioNote;
                if (priceKnown) {
                    char c[48];
                    snprintf(c, sizeof(c), ", %s$%.4f", r.upperBound ? "at most " : "about ", r.cost);
                    line += c;
                } else {
                    line += priceNote();
                }
                ImGui::TextUnformatted(line.c_str());
            } else {
                ImGui::TextDisabled("all indexed");
            }
            ImGui::TableNextColumn();
            if (r.count > 0) {
                ImGui::BeginDisabled(!canIndexType);
                char label[64];
                snprintf(label, sizeof(label), "Index %d##%s", r.count, r.name);
                if (ImGui::Button(label, ImVec2(-1, 0))) EmbedStartIndexing(false, r.type);
                ImGui::EndDisabled();
                mismatchTip();
                if (!mismatch && ImGui::IsItemHovered()) ImGui::SetTooltip("%s", r.tip);
            }
        }
        ImGui::EndTable();
    }

    if (todo > 0)
        ImGui::TextDisabled("~%.1f min at %d parallel.",
                            (embedUnindexedTex * 0.85 + embedUnindexedModel * 1.2 + embedUnindexedAudio * 2.5) / (std::max)(1, embedSettings.concurrency) / 60.0,
                            embedSettings.concurrency);
    else if (!running)
        ImGui::TextDisabled("Nothing to index.");

    if (running) {
        int done = embedIndexer.done.load(), failed = embedIndexer.failed.load(), total = embedIndexer.total.load();
        float frac = total ? (float)(done + failed) / total : 0.0f;
        char overlay[96];
        double elapsed = NowSeconds() - embedIndexer.startTime;
        double rate = elapsed > 0 ? (done + failed) / elapsed : 0;
        double eta = rate > 0 ? (total - done - failed) / rate : 0;
        snprintf(overlay, sizeof(overlay), "%d / %d   failed %d   ETA %.0fs", done, total, failed, eta);
        ImGui::ProgressBar(frac, ImVec2(-1, 0), overlay);
        ImGui::Text("Spent so far: $%.4f", embedIndexer.costSoFar());
        if (ImGui::Button("Stop")) embedIndexer.Cancel();
        ImGui::SameLine();
        ImGui::TextDisabled("Progress is kept.");
    } else {
        ImGui::BeginDisabled(!canIndex || todo == 0);
        char label[64];
        snprintf(label, sizeof(label), mismatch ? "Rebuild index (%d items)" : "Index everything (%d)", todo);
        if (ImGui::Button(label)) {
            if (mismatch) { embedConfirmRebuild = true; embedRebuildTypes = EmbedAll; }
            else EmbedStartIndexing(false, EmbedAll);
        }
        ImGui::EndDisabled();
        if (embedIndexLoaded && !embedIndex.Empty() && !mismatch) {
            ImGui::SameLine();
            ImGui::BeginDisabled(!canIndex);
            if (ImGui::Button("Rebuild from scratch")) { embedConfirmRebuild = true; embedRebuildTypes = EmbedAll; }
            ImGui::EndDisabled();
        }
        if (embedConfirmRebuild) ImGui::OpenPopup("Rebuild embedding index?");
        if (ImGui::BeginPopupModal("Rebuild embedding index?", nullptr, ImGuiWindowFlags_AlwaysAutoResize)) {
            embedConfirmRebuild = false;
            size_t n = embedIndex.entries.size();
            ImGui::Text("Discard %zu vectors and re-embed everything?", n);
            ImGui::TextDisabled("A backup is kept.");
            ImGui::Spacing();
            if (ImGui::Button("Rebuild", ImVec2(120, 0))) { EmbedStartIndexing(true, embedRebuildTypes); ImGui::CloseCurrentPopup(); }
            ImGui::SameLine();
            if (ImGui::Button("Cancel", ImVec2(120, 0))) ImGui::CloseCurrentPopup();
            ImGui::EndPopup();
        }
        if (db.scanning) { ImGui::SameLine(); ImGui::TextDisabled("(scanning)"); }
        if (embedIndexer.aborted)
            ImGui::TextColored(ImVec4(1, 0.4f, 0.4f, 1), "Stopped: %s", embedIndexer.abortReason.c_str());
        else if (embedIndexer.budgetHit)
            ImGui::TextColored(Theme::Accent(), "Last run: spending cap reached, %d embedded, $%.4f", embedIndexer.done.load(), embedIndexer.costSoFar());
        else if (embedIndexer.total > 0)
            ImGui::TextDisabled("Last run: %d embedded, %d failed, $%.4f", embedIndexer.done.load(),
                                embedIndexer.failed.load(), embedIndexer.costSoFar());
    }
    std::vector<std::string> errors = embedIndexer.RecentErrors();
    if (!errors.empty()) {
        if (ImGui::TreeNode("Errors", "Errors (%zu shown)", errors.size())) {
            for (const std::string& e : errors) ImGui::TextWrapped("%s", e.c_str());
            ImGui::TreePop();
        }
    }

    // ---- maintenance: nothing here happens without a click
    ImGui::SeparatorText("Maintenance");
    if (embedStaleCount > 0) {
        ImGui::AlignTextToFramePadding();
        ImGui::Text("%d file%s changed on disk since indexing", embedStaleCount, embedStaleCount == 1 ? "" : "s");
        ImGui::SameLine();
        ImGui::BeginDisabled(running);
        if (ImGui::Button("Keep vectors")) EmbedKeepStaleVectors();
        ImGui::EndDisabled();
        if (ImGui::IsItemHovered(ImGuiHoveredFlags_AllowWhenDisabled))
            ImGui::SetTooltip("Same content (e.g. copied library): keep instead of re-embedding");
    }
    if (embedMissingCount > 0) {
        ImGui::AlignTextToFramePadding();
        ImGui::Text("%d vector%s for deleted files", embedMissingCount, embedMissingCount == 1 ? "" : "s");
        ImGui::SameLine();
        ImGui::BeginDisabled(running || db.scanning);
        if (ImGui::Button("Remove")) {
            embedIndex.Compact(db.entries);
            embedCountedAt = -1;
        }
        ImGui::EndDisabled();
        if (ImGui::IsItemHovered(ImGuiHoveredFlags_AllowWhenDisabled))
            ImGui::SetTooltip("Hidden anyway; a backup is taken first");
    }
    {
        std::vector<std::wstring> backups = EmbedIndex::ListBackups(EmbedIndexPathForRoot());
        ImGui::BeginDisabled(running || !embedIndexLoaded || embedIndex.Empty());
        if (ImGui::Button("Back up now")) {
            std::wstring b = embedIndex.Backup();
            SemanticNotify(b.empty() ? "Backup failed." : "Backup written: " + WideToUtf8(fs::path(b).filename().wstring()));
        }
        ImGui::EndDisabled();
        if (!backups.empty()) {
            ImGui::SameLine();
            if (ImGui::TreeNode("Backups", "Backups (%zu)", backups.size())) {
                std::wstring restore;
                for (const std::wstring& b : backups) {
                    std::string name = WideToUtf8(fs::path(b).filename().wstring());
                    std::error_code ec;
                    double mb = fs::file_size(b, ec) / (1024.0 * 1024.0);
                    ImGui::PushID(name.c_str());
                    ImGui::BeginDisabled(running);
                    if (ImGui::SmallButton("Restore")) restore = b;
                    ImGui::EndDisabled();
                    ImGui::SameLine();
                    ImGui::Text("%s", name.c_str());
                    ImGui::SameLine();
                    ImGui::TextDisabled("%.1f MB", mb);
                    ImGui::PopID();
                }
                if (!restore.empty()) {
                    if (embedIndex.Restore(restore)) {
                        embedIndexLoaded = true;
                        semanticTextCache.clear();
                        SemanticClear();
                        embedCountedAt = -1;
                        SemanticNotify("Index restored from backup.");
                    } else {
                        SemanticNotify("Restore failed.");
                    }
                }
                ImGui::TreePop();
            }
        }
    }

    // ---- exclusions
    ImGui::SeparatorText("Excluded folders");
    if (embedExcludes.empty()) {
        ImGui::TextDisabled("None. Right-click a pack or asset to exclude its folder.");
    } else {
        std::string remove;
        for (const std::string& x : embedExcludes) {
            ImGui::PushID(x.c_str());
            if (ImGui::SmallButton("x")) remove = x;
            ImGui::SameLine();
            ImGui::TextUnformatted(x.c_str());
            ImGui::PopID();
        }
        if (!remove.empty()) EmbedSetFolderExcluded(remove, false);
    }
    ImGui::SetNextItemWidth(-80);
    bool addExclude = ImGui::InputTextWithHint("##exclude", "Packs/OldStuff",
                                               embedExcludeBuf, sizeof(embedExcludeBuf), ImGuiInputTextFlags_EnterReturnsTrue);
    ImGui::SameLine();
    if (ImGui::Button("Add", ImVec2(-1, 0))) addExclude = true;
    if (addExclude && embedExcludeBuf[0]) {
        EmbedSetFolderExcluded(embedExcludeBuf, true);
        embedExcludeBuf[0] = 0;
    }

    // ---- usage
    ImGui::SeparatorText("Searching");
    ImGui::TextDisabled("Semantic + Enter  \xC2\xB7  Ctrl+V / drop image or sound  \xC2\xB7  S: similar  \xC2\xB7  indexed:no finds the rest");
    if (ImGui::Button("Open index folder"))
        ShellExecuteW(nullptr, L"open", EmbedDir().c_str(), nullptr, nullptr, SW_SHOWNORMAL);
    if (!semanticError.empty()) ImGui::TextColored(ImVec4(1, 0.4f, 0.4f, 1), "%s", semanticError.c_str());

    ImGui::End();
}
