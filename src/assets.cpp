#include "assets.h"
#include "util.h"
#include <filesystem>
#include <unordered_map>
#include <algorithm>
#include <chrono>

namespace fs = std::filesystem;

namespace {

// Model format priority: lower = preferred canonical format.
int ModelPriority(const std::wstring& ext) {
    if (ext == L".glb") return 0;
    if (ext == L".gltf") return 1;
    if (ext == L".fbx") return 2;
    if (ext == L".obj") return 3;
    if (ext == L".dae") return 4;
    return 9;
}

enum class Kind { Skip, Texture, Model, Audio };

Kind Classify(const std::wstring& extLower) {
    static const wchar_t* tex[] = { L".png", L".jpg", L".jpeg", L".bmp", L".tga", L".gif", L".psd" };
    static const wchar_t* mdl[] = { L".glb", L".gltf", L".fbx", L".obj", L".dae" };
    static const wchar_t* aud[] = { L".wav", L".ogg", L".mp3", L".flac" };
    for (auto e : tex) if (extLower == e) return Kind::Texture;
    for (auto e : mdl) if (extLower == e) return Kind::Model;
    for (auto e : aud) if (extLower == e) return Kind::Audio;
    return Kind::Skip; // whitelist: everything else (.import/.meta/.blend/.zip/...) is ignored
}

// Packs export the same model into parallel format trees, e.g.
//   Models/GLB (recommended)/Decals/cobweb_1.glb
//   Models/other-formats/FBX/Decals/cobweb_1.fbx
// Build a dedupe key from the relative path with format-marker segments removed.
bool IsFormatSegment(std::wstring seg) {
    seg = ToLowerAsciiW(std::move(seg));
    if (seg == L"other-formats" || seg == L"other formats" || seg == L"collada") return true;
    static const wchar_t* fmts[] = { L"glb", L"gltf", L"fbx", L"obj", L"dae" };
    for (auto f : fmts) {
        size_t n = wcslen(f);
        if (seg.compare(0, n, f) == 0 &&
            (seg.size() == n || !iswalnum(seg[n]))) // "glb", "glb (recommended)" but not "objects"
            return true;
    }
    return false;
}

std::wstring ModelDedupeKey(const fs::path& relToRoot) {
    std::wstring key;
    fs::path dir = relToRoot.parent_path();
    for (const fs::path& seg : dir) {
        std::wstring s = seg.wstring();
        if (IsFormatSegment(s)) continue;
        key += ToLowerAsciiW(s);
        key += L'|';
    }
    key += ToLowerAsciiW(relToRoot.stem().wstring());
    return key;
}

uint64_t MtimeU64(const fs::directory_entry& de, std::error_code& ec) {
    auto t = de.last_write_time(ec);
    if (ec) return 0;
    auto systemTime = std::chrono::time_point_cast<std::chrono::seconds>(
        std::chrono::system_clock::now() + (t - fs::file_time_type::clock::now()));
    return (uint64_t)systemTime.time_since_epoch().count();
}

} // namespace

void AssetDatabase::StartScan(const std::wstring& rootDir) {
    root = rootDir;
    scanning = true;
    scanThread = std::thread([this] { ScanWorker(); });
}

void AssetDatabase::StopScan() {
    if (scanThread.joinable()) scanThread.join();
}

void AssetDatabase::ScanWorker() {
    std::vector<AssetEntry> batch;
    auto flushBatch = [&] {
        if (batch.empty()) return;
        std::lock_guard<std::mutex> lock(pendingMutex);
        for (auto& e : batch) pending.push_back(std::move(e));
        batch.clear();
    };

    // Models keyed by lowercase "dir|stem" so sibling exports of the same model
    // collapse into one entry. They are flushed after the walk completes (files of
    // one directory are not contiguous in a depth-first walk).
    struct ModelGroup { AssetEntry entry; int priority = 99; };
    std::unordered_map<std::wstring, ModelGroup> models;

    std::error_code ec;
    fs::recursive_directory_iterator it(root, fs::directory_options::skip_permission_denied, ec);
    fs::recursive_directory_iterator end;
    const fs::path rootPath(root);

    for (; it != end; it.increment(ec)) {
        if (ec) { ec.clear(); continue; }
        const fs::directory_entry& de = *it;
        if (de.is_directory(ec)) {
            std::wstring dn = de.path().filename().wstring();
            if (!dn.empty() && (dn[0] == L'!' || dn[0] == L'.'))
                it.disable_recursion_pending(); // skip "!Zips", hidden dirs
            continue;
        }
        if (!de.is_regular_file(ec)) continue;

        const fs::path& p = de.path();
        std::wstring extLower = ToLowerAsciiW(p.extension().wstring());
        Kind kind = Classify(extLower);
        if (kind == Kind::Skip) continue;

        AssetEntry e;
        e.path = p.wstring();
        e.ext = WideToUtf8(extLower);
        e.name = WideToUtf8(p.stem().wstring());
        fs::path rel = p.lexically_relative(rootPath); // lexical: avoids per-file syscalls
        e.relPath = WideToUtf8(rel.empty() ? p.filename().wstring() : rel.generic_wstring());
        e.nameLower = ToLowerAscii(e.name);
        e.relPathLower = ToLowerAscii(e.relPath);
        // pack = first component of the relative path
        size_t slash = e.relPath.find('/');
        e.pack = (slash == std::string::npos) ? std::string("(root)") : e.relPath.substr(0, slash);
        e.mtime = MtimeU64(de, ec);
        e.size = de.file_size(ec);

        if (kind == Kind::Model) {
            e.type = AssetType::Model;
            std::wstring key = ModelDedupeKey(rel);
            int prio = ModelPriority(extLower);
            auto [mit, inserted] = models.try_emplace(std::move(key));
            ModelGroup& g = mit->second;
            if (inserted) {
                g.entry = std::move(e);
                g.priority = prio;
            } else if (prio < g.priority) {
                g.entry.variants.push_back(g.entry.path);
                std::wstring newPath = e.path;
                auto vars = std::move(g.entry.variants);
                g.entry = std::move(e);
                g.entry.variants = std::move(vars);
                g.priority = prio;
            } else {
                g.entry.variants.push_back(std::move(e.path));
            }
            continue;
        }

        e.type = (kind == Kind::Texture) ? AssetType::Texture : AssetType::Audio;
        batch.push_back(std::move(e));
        if (batch.size() >= 256) flushBatch();
    }

    for (auto& [k, g] : models) {
        std::sort(g.entry.variants.begin(), g.entry.variants.end(),
                  [](const std::wstring& a, const std::wstring& b) {
                      auto extOf = [](const std::wstring& s) {
                          size_t d = s.find_last_of(L'.');
                          return ToLowerAsciiW(d == std::wstring::npos ? L"" : s.substr(d));
                      };
                      return ModelPriority(extOf(a)) < ModelPriority(extOf(b));
                  });
        batch.push_back(std::move(g.entry));
        if (batch.size() >= 256) flushBatch();
    }
    flushBatch();
    scanning = false;
}

bool AssetDatabase::PumpScan() {
    std::vector<AssetEntry> incoming;
    {
        std::lock_guard<std::mutex> lock(pendingMutex);
        if (pending.empty()) return false;
        incoming.swap(pending);
    }
    for (auto& e : incoming) {
        typeCounts[(int)e.type]++;
        auto pit = std::find_if(packs.begin(), packs.end(),
                                [&](const PackInfo& p) { return p.name == e.pack; });
        if (pit == packs.end()) {
            packs.push_back({ e.pack, 1 });
            std::sort(packs.begin(), packs.end(),
                      [](const PackInfo& a, const PackInfo& b) { return a.name < b.name; });
        } else {
            pit->count++;
        }
        entries.push_back(std::move(e));
    }
    return true;
}
