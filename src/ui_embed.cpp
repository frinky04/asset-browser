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

namespace fs = std::filesystem;

namespace {


std::wstring EmbedDir() { return AppDataDir(L"embeddings"); }
std::wstring SettingsFile() { return EmbedDir() + L"\\settings.json"; }
std::wstring KeyFile() { return EmbedDir() + L"\\openrouter.key"; }

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

std::wstring App::EmbedIndexPathForRoot() const {
    std::string utf8 = WideToUtf8(ToLowerAsciiW(db.root));
    wchar_t name[32];
    swprintf_s(name, L"%016llx.abe", (unsigned long long)Fnv1a64(utf8.data(), utf8.size()));
    return EmbedDir() + L"\\" + name;
}

void App::EmbedLoadIndexForRoot() {
    embedIndexLoaded = embedIndex.Load(EmbedIndexPathForRoot());
    embedPrunedAfterScan = false;
    // Query vectors only make sense against the index they were embedded for.
    semanticTextCache.clear();
    SemanticClear();
}

void App::EmbedShutdown() {
    SafeRelease(semanticImageSRV);
    embedIndexer.Cancel();
    embedIndexer.Join();
    if (semanticJob.thread.joinable()) semanticJob.thread.join();
    if (embedPriceThread.joinable()) embedPriceThread.join();
    if (embedKeyTestThread.joinable()) embedKeyTestThread.join();
    if (embedProbeThread.joinable()) embedProbeThread.join();
}

void App::PumpEmbed() {
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
        else
            snprintf(msg, sizeof(msg), "Indexed %d assets for $%.4f%s", embedIndexer.done.load(),
                     embedIndexer.costSoFar(), semanticSearch ? "." : ". Tick Semantic next to the search box to use it.");
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
        embedUnindexedTex = embedUnindexedAudio = embedAudioUnknown = 0;
        embedAudioSecondsKnown = 0;
        for (int i : EmbedUnindexed()) {
            const AssetEntry& e = db.entries[i];
            if (e.type != AssetType::Audio) { ++embedUnindexedTex; continue; }
            ++embedUnindexedAudio;
            auto it = audioDurations.find(e.relPath);
            if (it == audioDurations.end()) ++embedAudioUnknown;
            else embedAudioSecondsKnown += (std::min)(it->second, kEmbedAudioMaxSeconds);
        }
        embedUnindexedCount = embedUnindexedTex + embedUnindexedAudio;
    }
    if (embedWindowOpen) EmbedProbeDurations();
    // Prune index entries for files that vanished, once the scan has settled.
    if (embedIndexLoaded && !db.scanning && !embedPrunedAfterScan && !db.root.empty()) {
        embedPrunedAfterScan = true;
        if (!embedIndexer.Running()) embedIndex.Prune(db.entries);
    }
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
        if (!EmbedIsIndexable(e)) continue;
        if (!(types & (e.type == AssetType::Audio ? EmbedAudio : EmbedTex))) continue;
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

void App::EmbedStartIndexing(bool rebuild, int types) {
    if (embedIndexer.Running() || embedKey.empty() || db.root.empty()) return;
    embedIndexer.Join();
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
    embedIndexer.Start(&embedIndex, client, embedSettings.mode, embedSettings.concurrency, std::move(jobs),
        [th](const EmbedJob& j, std::vector<uint8_t>& data, const char*& mime) {
            if (j.type == AssetType::Audio) {
                PcmData pcm = DecodeAudioFile(j.path);
                mime = "audio/wav";
                return EncodeWav16kMono(pcm, 0, pcm.frames, data) > 0;
            }
            int w = 0, h = 0;
            std::vector<uint8_t> rgba;
            mime = "image/jpeg";
            return th->LoadOrMakeTextureThumb(j.path, j.mtime, j.size, w, h, rgba) && EncodeJpeg(rgba.data(), w, h, data);
        });
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
    double sec = EncodeWav16kMono(pcm, f0, f1, wav);
    if (sec <= 0) { SemanticNotify("Could not decode that audio."); return; }
    // waveform of exactly what is sent, as the query chip
    PcmData part;
    part.channels = pcm.channels; part.sampleRate = pcm.sampleRate; part.valid = true;
    uint64_t end = (std::min)(f1, f0 + (uint64_t)(kEmbedAudioMaxSeconds * pcm.sampleRate));
    part.frames = end - f0;
    part.samples.assign(pcm.samples.begin() + (size_t)f0 * pcm.channels, pcm.samples.begin() + (size_t)end * pcm.channels);
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
        semanticJob.kind = SemanticKind::Image;
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
        SemanticNotify("Clipboard has no image or file. Copy a screenshot region, an image, or a sound file, then Ctrl+V.");
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
    if (!ImGui::Begin("Embedding search", &embedWindowOpen)) { ImGui::End(); return; }

    bool running = embedIndexer.Running();

    // ---- status header
    {
        const char* state; ImVec4 col = Theme::Accent();
        if (embedKey.empty()) { state = "Not set up: add an OpenRouter API key below."; col = ImGui::GetStyleColorVec4(ImGuiCol_TextDisabled); }
        else if (db.root.empty()) { state = "Choose an asset folder to index."; col = ImGui::GetStyleColorVec4(ImGuiCol_TextDisabled); }
        else if (running) state = "Indexing...";
        else if (!embedIndexLoaded || embedIndex.Empty()) state = "Key ready. Index your textures and sounds to enable semantic search.";
        else if (embedUnindexedCount > 0) state = "Semantic search ready; some assets are not indexed yet.";
        else state = "Semantic search ready: every colour texture and sound is indexed.";
        ImGui::TextColored(col, "%s", state);
        ImGui::Spacing();
    }

    // ---- API key
    ImGui::SeparatorText("OpenRouter API key");
    if (embedKey.empty())
        ImGui::TextDisabled("No key set. Paste a key or set OPENROUTER_API_KEY.");
    else if (embedKeyFromEnv)
        ImGui::TextDisabled("Using key from OPENROUTER_API_KEY environment variable.");
    else
        ImGui::TextDisabled("Key stored (encrypted for this Windows user).");
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
    static const char* modeNames[] = { "Hybrid (image + filename)", "Image only" };
    if (ImGui::Combo("Input", &mode, modeNames, 2)) { embedSettings.mode = (EmbedMode)mode; changed = true; }
    ImGui::SameLine();
    ImGui::TextDisabled("(?)");
    if (ImGui::IsItemHovered())
        ImGui::SetTooltip("Hybrid sends the filename words with the image; much better for text\n"
                          "queries when files are named sensibly. Image only is purer for image queries.");
    ImGui::SetNextItemWidth(180);
    int dimsIdx = embedSettings.dims == 3072 ? 1 : 0;
    static const char* dimNames[] = { "768 (compact)", "3072 (full)" };
    if (ImGui::Combo("Dimensions", &dimsIdx, dimNames, 2)) { embedSettings.dims = dimsIdx ? 3072 : 768; changed = true; }
    ImGui::SetNextItemWidth(180);
    if (ImGui::SliderInt("Parallel requests", &embedSettings.concurrency, 1, 16)) changed = true;
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

    if (embedIndexLoaded && !embedIndex.Empty())
        ImGui::Text("Index: %zu vectors, %s, %s, %d dims", embedIndex.entries.size(), embedIndex.model.c_str(),
                    embedIndex.mode == EmbedMode::Hybrid ? "hybrid" : "media only", embedIndex.dims);
    else
        ImGui::TextDisabled("No index for this library yet.");
    if (mismatch)
        ImGui::TextColored(Theme::Accent(), "Settings differ from the index. Indexing will rebuild it from scratch.");

    bool priceKnown = embedPrices.image > 0 || embedPrices.audio > 0;
    auto priceNote = [&]() -> std::string {
        if (priceKnown) return "";
        if (embedPriceFetching) return " (fetching price...)";
        return embedPriceError.empty() ? " (price unknown)" : " (price unknown: " + embedPriceError + ")";
    };
    bool canIndex = !embedKey.empty() && !db.scanning && !running;

    // -- textures
    double texCost = embedUnindexedTex * (double)kEmbedTokensPerImage * embedPrices.image;
    ImGui::AlignTextToFramePadding();
    if (embedUnindexedTex > 0) {
        if (priceKnown) ImGui::Text("Textures: %d to index, about $%.4f", embedUnindexedTex, texCost);
        else ImGui::Text("Textures: %d to index%s", embedUnindexedTex, priceNote().c_str());
    } else {
        ImGui::TextDisabled("Textures: all indexed (colour maps; normal/roughness/etc. are skipped)");
    }
    if (embedUnindexedTex > 0) {
        ImGui::SameLine();
        ImGui::BeginDisabled(!canIndex);
        char label[64];
        snprintf(label, sizeof(label), "Index %d textures##tex", embedUnindexedTex);
        if (ImGui::Button(label)) EmbedStartIndexing(mismatch, EmbedTex);
        ImGui::EndDisabled();
    }

    // -- sounds: 25 tokens per second, first 12 s of each clip
    double audioSeconds = embedAudioSecondsKnown + embedAudioUnknown * kEmbedAudioMaxSeconds; // unknown = upper bound
    double audioCost = audioSeconds * kEmbedTokensPerAudioSecond * embedPrices.audio;
    ImGui::AlignTextToFramePadding();
    if (embedUnindexedAudio > 0) {
        char secs[64];
        if (audioSeconds >= 3600) snprintf(secs, sizeof(secs), "%.1f h", audioSeconds / 3600);
        else if (audioSeconds >= 60) snprintf(secs, sizeof(secs), "%.1f min", audioSeconds / 60);
        else snprintf(secs, sizeof(secs), "%.0f s", audioSeconds);
        if (priceKnown) ImGui::Text("Sounds: %d to index, %s of audio, %s$%.4f", embedUnindexedAudio, secs,
                                    embedAudioUnknown ? "at most " : "about ", audioCost);
        else ImGui::Text("Sounds: %d to index, %s of audio%s", embedUnindexedAudio, secs, priceNote().c_str());
        if (embedAudioUnknown) {
            ImGui::SameLine();
            ImGui::TextDisabled("(measuring %d clips...)", embedAudioUnknown);
        }
    } else {
        ImGui::TextDisabled("Sounds: all indexed");
    }
    if (embedUnindexedAudio > 0) {
        ImGui::SameLine();
        ImGui::BeginDisabled(!canIndex);
        char label[64];
        snprintf(label, sizeof(label), "Index %d sounds##audio", embedUnindexedAudio);
        if (ImGui::Button(label)) EmbedStartIndexing(mismatch, EmbedAudio);
        ImGui::EndDisabled();
    }
    if (ImGui::IsItemHovered() && embedUnindexedAudio > 0)
        ImGui::SetTooltip("Only the first %.0f seconds of each clip are embedded (16 kHz mono).\n"
                          "Audio costs about 14x more per item than textures.", kEmbedAudioMaxSeconds);
    if (todo > 0)
        ImGui::TextDisabled("Roughly %.1f min at %d parallel requests.",
                            (embedUnindexedTex * 0.85 + embedUnindexedAudio * 2.5) / (std::max)(1, embedSettings.concurrency) / 60.0,
                            embedSettings.concurrency);
    else if (!running)
        ImGui::TextDisabled("Everything is indexed.");

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
        if (ImGui::Button("Cancel")) embedIndexer.Cancel();
    } else {
        ImGui::BeginDisabled(!canIndex || todo == 0);
        char label[64];
        snprintf(label, sizeof(label), mismatch ? "Rebuild index (%d items)" : "Index everything (%d)", todo);
        if (ImGui::Button(label)) EmbedStartIndexing(mismatch, EmbedAll);
        ImGui::EndDisabled();
        if (embedIndexLoaded && !embedIndex.Empty() && !mismatch) {
            ImGui::SameLine();
            ImGui::BeginDisabled(!canIndex);
            if (ImGui::Button("Rebuild from scratch")) EmbedStartIndexing(true, EmbedAll);
            ImGui::EndDisabled();
        }
        if (embedKey.empty()) { ImGui::SameLine(); ImGui::TextDisabled("(needs an API key)"); }
        else if (db.scanning) { ImGui::SameLine(); ImGui::TextDisabled("(wait for the scan to finish)"); }
        if (embedIndexer.aborted)
            ImGui::TextColored(ImVec4(1, 0.4f, 0.4f, 1), "Stopped: %s", embedIndexer.abortReason.c_str());
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

    // ---- usage
    ImGui::SeparatorText("Searching");
    ImGui::BulletText("Tick Semantic next to the search box, describe a texture, press Enter.");
    ImGui::BulletText("Ctrl+V a screenshot region, or drop / paste an image or sound file onto the window.");
    ImGui::BulletText("Select a texture or sound and press S, or right-click > Find similar.");
    ImGui::BulletText("In the audio preview, select a range and click Search similar.");
    ImGui::BulletText("type:, pack:, ext: and the other filters still narrow the ranking.");
    if (ImGui::Button("Open index folder"))
        ShellExecuteW(nullptr, L"open", EmbedDir().c_str(), nullptr, nullptr, SW_SHOWNORMAL);
    if (!semanticError.empty()) ImGui::TextColored(ImVec4(1, 0.4f, 0.4f, 1), "%s", semanticError.c_str());

    ImGui::End();
}
