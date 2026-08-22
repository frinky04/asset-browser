#include "embed.h"
#include "util.h"
#include <windows.h>
#include <winhttp.h>
#include <wincrypt.h>
#include <nlohmann/json.hpp>
#include <algorithm>
#include <chrono>
#include <cmath>
#include <cstdio>
#include <filesystem>
#include <fstream>

#include "stb_image_write.h"

namespace fs = std::filesystem;
using json = nlohmann::json;

// ---------------------------------------------------------------- settings

void EmbedSettings::Load(const std::wstring& file) {
    std::ifstream in(file, std::ios::binary);
    if (!in) return;
    json j = json::parse(in, nullptr, /*allow_exceptions*/ false);
    if (!j.is_object()) return;
    model = j.value("model", model);
    mode = j.value("mode", 0) == 1 ? EmbedMode::ImageOnly : EmbedMode::Hybrid;
    dims = j.value("dims", 768) == 3072 ? 3072 : 768;
    concurrency = (std::max)(1, (std::min)(16, j.value("concurrency", 8)));
    budgetUsd = (std::max)(0.0, j.value("budgetUsd", 0.0));
}

void EmbedSettings::Save(const std::wstring& file) const {
    std::error_code ec;
    fs::create_directories(fs::path(file).parent_path(), ec);
    json j = { {"model", model}, {"mode", (int)mode}, {"dims", dims}, {"concurrency", concurrency}, {"budgetUsd", budgetUsd} };
    std::ofstream out(file, std::ios::binary | std::ios::trunc);
    out << j.dump(2);
}

// ---------------------------------------------------------------- index file
// Layout: header { 'ABE1', u32 dims, u8 mode, u16 modelLen, model } then
// records { u16 relLen, rel, u64 mtime, u64 size, float[dims] } appended as
// vectors arrive. A truncated trailing record (crash mid-write) is ignored.

namespace {
constexpr uint32_t kIndexMagic = 0x31454241; // "ABE1"

void Normalise(float* v, int n) {
    double s = 0;
    for (int i = 0; i < n; ++i) s += (double)v[i] * v[i];
    float inv = s > 0 ? (float)(1.0 / std::sqrt(s)) : 0.0f;
    for (int i = 0; i < n; ++i) v[i] *= inv;
}

template <class T> bool ReadPod(const std::vector<uint8_t>& b, size_t& pos, T& out) {
    if (pos + sizeof(T) > b.size()) return false;
    memcpy(&out, b.data() + pos, sizeof(T));
    pos += sizeof(T);
    return true;
}

void WriteHeader(FILE* f, const std::string& model, EmbedMode mode, int dims) {
    uint32_t magic = kIndexMagic, d = (uint32_t)dims;
    uint8_t m = (uint8_t)mode;
    uint16_t len = (uint16_t)model.size();
    fwrite(&magic, 4, 1, f); fwrite(&d, 4, 1, f); fwrite(&m, 1, 1, f);
    fwrite(&len, 2, 1, f); fwrite(model.data(), 1, len, f);
}

void WriteRecord(FILE* f, const std::string& rel, uint64_t mtime, uint64_t size, const float* v, int dims) {
    uint16_t len = (uint16_t)(std::min)(rel.size(), (size_t)65535);
    fwrite(&len, 2, 1, f); fwrite(rel.data(), 1, len, f);
    fwrite(&mtime, 8, 1, f); fwrite(&size, 8, 1, f);
    fwrite(v, sizeof(float), dims, f);
}
} // namespace

namespace {
std::wstring TimestampW() {
    SYSTEMTIME st; GetLocalTime(&st);
    wchar_t buf[32];
    swprintf_s(buf, L"%04d%02d%02d-%02d%02d%02d%03d", st.wYear, st.wMonth, st.wDay, st.wHour, st.wMinute, st.wSecond, st.wMilliseconds);
    return buf;
}
// Never overwrite an existing backup: suffix until the name is free.
std::wstring UniquePath(const std::wstring& base, const std::wstring& ext) {
    std::error_code ec;
    std::wstring p = base + ext;
    for (int i = 2; fs::exists(p, ec); ++i) p = base + L"-" + std::to_wstring(i) + ext;
    return p;
}
// Replace `dst` with `tmp` atomically (MoveFileEx with REPLACE_EXISTING is a rename on NTFS).
bool SwapIn(const std::wstring& tmp, const std::wstring& dst) {
    return MoveFileExW(tmp.c_str(), dst.c_str(), MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH) != 0;
}
}

std::wstring EmbedIndex::Backup() const {
    std::error_code ec;
    if (file.empty() || !fs::exists(file, ec) || fs::file_size(file, ec) == 0) return L"";
    std::wstring dst = UniquePath(file + L"." + TimestampW(), L".bak");
    if (!CopyFileW(file.c_str(), dst.c_str(), TRUE)) return L"";
    // keep the newest 5 backups
    std::vector<std::wstring> all = ListBackups(file);
    for (size_t i = 5; i < all.size(); ++i) fs::remove(all[i], ec);
    return dst;
}

std::vector<std::wstring> EmbedIndex::ListBackups(const std::wstring& indexPath) {
    std::vector<std::wstring> out;
    std::error_code ec;
    fs::path base(indexPath);
    std::wstring stem = base.filename().wstring() + L".";
    for (auto& de : fs::directory_iterator(base.parent_path(), ec)) {
        std::wstring name = de.path().filename().wstring();
        if (name.rfind(stem, 0) == 0 && name.size() > 4 && name.compare(name.size() - 4, 4, L".bak") == 0)
            out.push_back(de.path().wstring());
    }
    std::sort(out.rbegin(), out.rend()); // timestamped names: lexical = chronological
    return out;
}

bool EmbedIndex::Restore(const std::wstring& backup) {
    if (file.empty()) return false;
    Backup();
    std::wstring tmp = file + L".tmp";
    if (!CopyFileW(backup.c_str(), tmp.c_str(), FALSE) || !SwapIn(tmp, file)) return false;
    return Load(file);
}

bool EmbedIndex::Load(const std::wstring& path) {
    loadedFromBackup = false;
    if (LoadFile(path)) return true;
    // Unreadable or corrupt: fall back to the newest backup that parses,
    // preferring one that actually holds vectors over an empty post-reset one.
    std::error_code ec;
    bool existed = fs::exists(path, ec) && fs::file_size(path, ec) > 0;
    std::vector<std::wstring> backups = ListBackups(path);
    std::wstring chosen;
    for (const std::wstring& b : backups)
        if (LoadFile(b) && !entries.empty()) { chosen = b; break; }
    if (chosen.empty())
        for (const std::wstring& b : backups)
            if (LoadFile(b)) { chosen = b; break; }
    if (!chosen.empty()) {
        const std::wstring& b = chosen;
        file = path; // keep appending to the real index path
        if (existed) {
            // preserve the corrupt file for forensics, then reinstate the backup
            fs::rename(path, UniquePath(path + L"." + TimestampW(), L".corrupt"), ec);
        }
        std::wstring tmp = path + L".tmp";
        if (CopyFileW(b.c_str(), tmp.c_str(), FALSE)) SwapIn(tmp, path);
        loadedFromBackup = true;
        return true;
    }
    return false;
}

bool EmbedIndex::LoadFile(const std::wstring& path) {
    { std::lock_guard<std::mutex> lock(fileMutex); pending.clear(); } // stale appends from a previous library
    entries.clear(); vectors.clear(); model.clear(); dims = 0; file = path;
    std::vector<uint8_t> b;
    if (!ReadFileBytes(path, b)) return false;
    size_t pos = 0;
    uint32_t magic = 0, d = 0; uint8_t m = 0; uint16_t len = 0;
    if (!ReadPod(b, pos, magic) || magic != kIndexMagic || !ReadPod(b, pos, d) ||
        !ReadPod(b, pos, m) || !ReadPod(b, pos, len) || pos + len > b.size())
        return false;
    if (d == 0 || d > 8192) return false;
    model.assign((const char*)b.data() + pos, len); pos += len;
    mode = (EmbedMode)m; dims = (int)d;
    for (;;) {
        size_t start = pos;
        uint16_t rl = 0; uint64_t mt = 0, sz = 0;
        if (!ReadPod(b, pos, rl) || pos + rl > b.size()) { pos = start; break; }
        std::string rel((const char*)b.data() + pos, rl); pos += rl;
        if (!ReadPod(b, pos, mt) || !ReadPod(b, pos, sz) || pos + (size_t)dims * 4 > b.size()) { pos = start; break; }
        auto it = entries.find(rel);
        size_t slot;
        if (it != entries.end()) slot = it->second.slot;           // re-indexed: overwrite
        else { slot = vectors.size() / dims; vectors.resize(vectors.size() + dims); }
        memcpy(vectors.data() + slot * dims, b.data() + pos, (size_t)dims * 4);
        pos += (size_t)dims * 4;
        entries[rel] = { mt, sz, slot };
    }
    return true;
}

void EmbedIndex::Reset(const std::string& mdl, EmbedMode md, int d, const std::wstring& path) {
    file = path;
    Backup(); // the old vectors survive as a timestamped .bak
    std::lock_guard<std::mutex> lock(fileMutex);
    entries.clear(); vectors.clear(); pending.clear();
    model = mdl; mode = md; dims = d;
    std::error_code ec;
    fs::create_directories(fs::path(path).parent_path(), ec);
    std::wstring tmp = path + L".tmp";
    FILE* f = _wfopen(tmp.c_str(), L"wb");
    if (!f) return;
    WriteHeader(f, model, mode, dims);
    fclose(f);
    SwapIn(tmp, path);
}

void EmbedIndex::Append(const std::string& relPath, uint64_t mtime, uint64_t size, const float* vec) {
    std::lock_guard<std::mutex> lock(fileMutex);
    FILE* f = _wfopen(file.c_str(), L"ab");
    if (f) { WriteRecord(f, relPath, mtime, size, vec, dims); fclose(f); }
    Pending p{ relPath, mtime, size, std::vector<float>(vec, vec + dims) };
    pending.push_back(std::move(p));
}

bool EmbedIndex::Pump() {
    std::vector<Pending> batch;
    { std::lock_guard<std::mutex> lock(fileMutex); batch.swap(pending); }
    for (Pending& p : batch) {
        auto it = entries.find(p.relPath);
        size_t slot;
        if (it != entries.end()) slot = it->second.slot;
        else { slot = vectors.size() / dims; vectors.resize(vectors.size() + dims); }
        memcpy(vectors.data() + slot * dims, p.vec.data(), (size_t)dims * 4);
        entries[p.relPath] = { p.mtime, p.size, slot };
    }
    return !batch.empty();
}

int EmbedIndex::CountMissing(const std::vector<AssetEntry>& db) const {
    std::unordered_map<std::string, bool> live;
    live.reserve(db.size());
    for (const AssetEntry& e : db) live[e.relPath] = true;
    int n = 0;
    for (auto& [rel, e] : entries) if (!live.count(rel)) ++n;
    return n;
}

void EmbedIndex::Compact(const std::vector<AssetEntry>& db) {
    std::unordered_map<std::string, bool> live;
    live.reserve(db.size());
    for (const AssetEntry& e : db) live[e.relPath] = true;
    size_t before = entries.size();
    for (auto it = entries.begin(); it != entries.end();)
        it = live.count(it->first) ? std::next(it) : entries.erase(it);
    if (entries.size() != before) { Backup(); Rewrite(); }
}

bool EmbedIndex::IsStale(const AssetEntry& e) const {
    auto it = entries.find(e.relPath);
    return it != entries.end() && (it->second.mtime != e.mtime || it->second.size != e.size);
}

void EmbedIndex::Adopt(const AssetEntry& e) {
    auto it = entries.find(e.relPath);
    if (it == entries.end()) return;
    it->second.mtime = e.mtime;
    it->second.size = e.size;
    // append a record with the new key; Load() keeps the last record per path
    std::lock_guard<std::mutex> lock(fileMutex);
    FILE* f = _wfopen(file.c_str(), L"ab");
    if (f) { WriteRecord(f, e.relPath, e.mtime, e.size, vectors.data() + it->second.slot * dims, dims); fclose(f); }
}

// Write the in-memory index to <file>.tmp, then swap it in. A crash mid-write
// leaves the old file untouched.
void EmbedIndex::Rewrite() {
    std::lock_guard<std::mutex> lock(fileMutex);
    std::vector<float> packed;
    packed.reserve(entries.size() * dims);
    std::wstring tmp = file + L".tmp";
    FILE* f = _wfopen(tmp.c_str(), L"wb");
    if (!f) return;
    WriteHeader(f, model, mode, dims);
    for (auto& [rel, e] : entries) {
        const float* v = vectors.data() + e.slot * dims;
        WriteRecord(f, rel, e.mtime, e.size, v, dims);
        e.slot = packed.size() / dims;
        packed.insert(packed.end(), v, v + dims);
    }
    bool ok = fflush(f) == 0;
    fclose(f);
    if (!ok || !SwapIn(tmp, file)) {
        std::error_code ec; fs::remove(tmp, ec);
        return; // keep the old file and the old slots
    }
    vectors.swap(packed);
}

bool EmbedIndex::HasCurrent(const AssetEntry& e) const {
    auto it = entries.find(e.relPath);
    return it != entries.end() && it->second.mtime == e.mtime && it->second.size == e.size;
}

const float* EmbedIndex::VectorFor(const AssetEntry& e) const {
    auto it = entries.find(e.relPath);
    if (it == entries.end() || it->second.mtime != e.mtime || it->second.size != e.size) return nullptr;
    return vectors.data() + it->second.slot * dims;
}

// ---------------------------------------------------------------- filename text

bool EmbedModelRenderable(const std::string& ext) {
    return ext == ".glb" || ext == ".gltf" || ext == ".fbx" || ext == ".obj";
}

bool EmbedIsIndexable(const AssetEntry& e) {
    if (e.type == AssetType::Audio) return true;
    if (e.type == AssetType::Model) return EmbedModelRenderable(e.ext);
    if (e.type != AssetType::Texture) return false;
    static const char* aux[] = { "normal", "rough", "metal", "occlusion", "_ao", "height",
                                 "displace", "emiss", "opacity", "specular", "gloss", "bump",
                                 "_orm", "cavity", "curvature" };
    for (auto a : aux)
        if (e.nameLower.find(a) != std::string::npos) return false;
    return true;
}

// Port of the research fname_to_text(): split CamelCase / digit boundaries,
// separators to spaces, drop pure-number tokens, lowercase.
std::string EmbedFilenameWords(const std::string& stem) {
    std::string s;
    auto lower = [](unsigned char c) { return c >= 'a' && c <= 'z'; };
    auto upper = [](unsigned char c) { return c >= 'A' && c <= 'Z'; };
    auto digit = [](unsigned char c) { return c >= '0' && c <= '9'; };
    for (size_t i = 0; i < stem.size(); ++i) {
        unsigned char c = stem[i];
        if (c == '-' || c == '_') { s += ' '; continue; }
        if (i > 0) {
            unsigned char p = stem[i - 1];
            if (lower(p) && (upper(c) || digit(c))) s += ' ';
            else if (upper(p) && upper(c) && i + 1 < stem.size() && lower((unsigned char)stem[i + 1])) s += ' ';
        }
        s += (char)std::tolower(c);
    }
    // tokens: drop \d+[a-z]? ; collapse whitespace
    std::string out, tok;
    auto flush = [&] {
        if (tok.empty()) return;
        size_t n = 0;
        while (n < tok.size() && digit((unsigned char)tok[n])) ++n;
        bool numeric = n > 0 && (n == tok.size() || (n + 1 == tok.size() && lower((unsigned char)tok[n])));
        if (!numeric) { if (!out.empty()) out += ' '; out += tok; }
        tok.clear();
    };
    for (char c : s) { if (c == ' ') flush(); else tok += c; }
    flush();
    return out;
}

std::string EmbedHybridText(AssetType type, const std::string& relPath, const std::string& name) {
    std::string words = EmbedFilenameWords(name);
    if (type == AssetType::Audio || type == AssetType::Model) {
        // Folder names in SFX packs are category labels ("Doors & Gates", "UI"):
        // the research showed they rescue text queries for short, generic sounds.
        std::string folder;
        size_t slash = relPath.rfind('/');
        if (slash != std::string::npos) {
            size_t prev = relPath.rfind('/', slash - 1);
            folder = relPath.substr(prev == std::string::npos ? 0 : prev + 1, slash - (prev == std::string::npos ? 0 : prev + 1));
        }
        std::string folderWords = EmbedFilenameWords(folder);
        std::string all = folderWords;
        if (!words.empty()) { if (!all.empty()) all += ' '; all += words; }
        const char* kind = type == AssetType::Audio ? "sound" : "3d model";
        return all.empty() ? kind : std::string(kind) + ": " + all;
    }
    return words.empty() ? "texture" : "texture: " + words;
}

double EncodeWav16kMono(const PcmData& pcm, uint64_t f0, uint64_t f1, std::vector<uint8_t>& out) {
    out.clear();
    if (!pcm.valid || pcm.channels == 0 || pcm.sampleRate == 0) return 0;
    f0 = (std::min)(f0, pcm.frames);
    f1 = (std::min)(f1, pcm.frames);
    if (f1 <= f0) return 0;
    uint64_t maxFrames = (uint64_t)(kEmbedAudioMaxSeconds * pcm.sampleRate);
    if (f1 - f0 > maxFrames) f1 = f0 + maxFrames;

    const uint32_t outRate = 16000;
    uint64_t srcFrames = f1 - f0;
    uint64_t outFrames = (uint64_t)((double)srcFrames * outRate / pcm.sampleRate);
    if (outFrames == 0) return 0;
    std::vector<int16_t> mono(outFrames);
    const float* src = pcm.samples.data() + (size_t)f0 * pcm.channels;
    double step = (double)pcm.sampleRate / outRate;
    for (uint64_t i = 0; i < outFrames; ++i) {
        // linear interpolation between neighbouring source frames, channels averaged
        double pos = i * step;
        uint64_t a = (uint64_t)pos;
        uint64_t b = (std::min)(a + 1, srcFrames - 1);
        float t = (float)(pos - a);
        float va = 0, vb = 0;
        for (uint32_t c = 0; c < pcm.channels; ++c) {
            va += src[a * pcm.channels + c];
            vb += src[b * pcm.channels + c];
        }
        float v = (va + (vb - va) * t) / pcm.channels;
        v = (std::max)(-1.0f, (std::min)(1.0f, v));
        mono[i] = (int16_t)(v * 32767.0f);
    }
    uint32_t dataBytes = (uint32_t)(outFrames * 2);
    uint32_t riffSize = 36 + dataBytes, fmtSize = 16, byteRate = outRate * 2;
    uint16_t fmt16 = 1, channels = 1, blockAlign = 2, bits = 16;
    auto put = [&](const void* p, size_t n) { out.insert(out.end(), (const uint8_t*)p, (const uint8_t*)p + n); };
    put("RIFF", 4); put(&riffSize, 4); put("WAVE", 4);
    put("fmt ", 4); put(&fmtSize, 4); put(&fmt16, 2); put(&channels, 2);
    put(&outRate, 4); put(&byteRate, 4); put(&blockAlign, 2); put(&bits, 2);
    put("data", 4); put(&dataBytes, 4); put(mono.data(), dataBytes);
    return (double)outFrames / outRate;
}

// ---------------------------------------------------------------- http

namespace {

struct HttpResponse { bool ok = false; int status = 0; std::string body, error; };

HttpResponse HttpRequest(const wchar_t* verb, const std::wstring& path, const std::string& apiKey,
                         const std::string& body) {
    HttpResponse r;
    HINTERNET session = WinHttpOpen(L"AssetBrowser/1.0", WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
                                    WINHTTP_NO_PROXY_NAME, WINHTTP_NO_PROXY_BYPASS, 0);
    if (!session) { r.error = "WinHttpOpen failed"; return r; }
    WinHttpSetTimeouts(session, 15000, 15000, 60000, 90000);
    HINTERNET conn = WinHttpConnect(session, L"openrouter.ai", INTERNET_DEFAULT_HTTPS_PORT, 0);
    HINTERNET req = conn ? WinHttpOpenRequest(conn, verb, path.c_str(), nullptr, WINHTTP_NO_REFERER,
                                              WINHTTP_DEFAULT_ACCEPT_TYPES, WINHTTP_FLAG_SECURE) : nullptr;
    if (!req) {
        r.error = "WinHttpOpenRequest failed (" + std::to_string(GetLastError()) + ")";
    } else {
        std::wstring headers = L"Content-Type: application/json\r\nX-Title: Asset Browser\r\n";
        if (!apiKey.empty()) headers += L"Authorization: Bearer " + Utf8ToWide(apiKey) + L"\r\n";
        BOOL ok = WinHttpSendRequest(req, headers.c_str(), (DWORD)-1,
                                     body.empty() ? WINHTTP_NO_REQUEST_DATA : (LPVOID)body.data(),
                                     (DWORD)body.size(), (DWORD)body.size(), 0) &&
                  WinHttpReceiveResponse(req, nullptr);
        if (!ok) {
            r.error = "network error (" + std::to_string(GetLastError()) + ")";
        } else {
            DWORD status = 0, len = sizeof(status);
            WinHttpQueryHeaders(req, WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
                                WINHTTP_HEADER_NAME_BY_INDEX, &status, &len, WINHTTP_NO_HEADER_INDEX);
            r.status = (int)status;
            for (;;) {
                DWORD avail = 0;
                if (!WinHttpQueryDataAvailable(req, &avail) || avail == 0) break;
                size_t off = r.body.size();
                r.body.resize(off + avail);
                DWORD rd = 0;
                if (!WinHttpReadData(req, r.body.data() + off, avail, &rd)) break;
                r.body.resize(off + rd);
            }
            r.ok = true;
        }
    }
    if (req) WinHttpCloseHandle(req);
    if (conn) WinHttpCloseHandle(conn);
    WinHttpCloseHandle(session);
    return r;
}

std::string Base64(const uint8_t* d, size_t n) {
    static const char* t = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    std::string o; o.reserve((n + 2) / 3 * 4);
    for (size_t i = 0; i < n; i += 3) {
        uint32_t v = d[i] << 16 | (i + 1 < n ? d[i + 1] << 8 : 0) | (i + 2 < n ? d[i + 2] : 0);
        o += t[v >> 18 & 63]; o += t[v >> 12 & 63];
        o += i + 1 < n ? t[v >> 6 & 63] : '=';
        o += i + 2 < n ? t[v & 63] : '=';
    }
    return o;
}

std::string ApiErrorMessage(const std::string& body, int status) {
    json j = json::parse(body, nullptr, false);
    if (j.is_object() && j.contains("error")) {
        const json& e = j["error"];
        if (e.is_object() && e.contains("message") && e["message"].is_string()) return e["message"].get<std::string>();
        if (e.is_string()) return e.get<std::string>();
    }
    return "HTTP " + std::to_string(status) + (body.empty() ? "" : ": " + body.substr(0, 200));
}

} // namespace

bool EncodeJpeg(const uint8_t* rgba, int w, int h, std::vector<uint8_t>& out, int quality) {
    out.clear();
    // stb jpeg takes RGB; strip alpha (composited on black would darken cutouts; plain drop is fine)
    std::vector<uint8_t> rgb((size_t)w * h * 3);
    for (size_t i = 0, n = (size_t)w * h; i < n; ++i) {
        rgb[i * 3] = rgba[i * 4]; rgb[i * 3 + 1] = rgba[i * 4 + 1]; rgb[i * 3 + 2] = rgba[i * 4 + 2];
    }
    auto sink = [](void* ctx, void* data, int size) {
        auto* v = (std::vector<uint8_t>*)ctx;
        v->insert(v->end(), (uint8_t*)data, (uint8_t*)data + size);
    };
    return stbi_write_jpg_to_func(sink, &out, w, h, 3, rgb.data(), quality) != 0;
}

// ---------------------------------------------------------------- client

EmbedResult EmbedClient::Call(const std::string& body) const {
    EmbedResult res;
    int delayMs = 1000;
    for (int attempt = 0; attempt < 5; ++attempt) {
        HttpResponse r = HttpRequest(L"POST", L"/api/v1/embeddings", apiKey, body);
        res.httpStatus = r.status;
        bool retry = !r.ok || r.status == 429 || r.status >= 500;
        if (r.ok && r.status == 200) {
            json j = json::parse(r.body, nullptr, false);
            if (!j.is_object() || !j.contains("data") || !j["data"].is_array() || j["data"].empty() ||
                !j["data"][0].contains("embedding") || !j["data"][0]["embedding"].is_array()) {
                res.error = "unexpected response: " + r.body.substr(0, 200);
                return res;
            }
            const json& emb = j["data"][0]["embedding"];
            res.vec.reserve(emb.size());
            for (const json& x : emb) res.vec.push_back(x.is_number() ? x.get<float>() : 0.0f);
            if ((int)res.vec.size() < dims) { res.error = "model returned only " + std::to_string(res.vec.size()) + " dims"; return res; }
            res.vec.resize(dims);               // Matryoshka truncation if the API ignored `dimensions`
            Normalise(res.vec.data(), dims);
            if (j.contains("usage") && j["usage"].is_object() && j["usage"].contains("cost") && j["usage"]["cost"].is_number())
                res.cost = j["usage"]["cost"].get<double>();
            res.ok = true;
            return res;
        }
        res.error = r.ok ? ApiErrorMessage(r.body, r.status) : r.error;
        if (!retry) return res;
        Sleep(delayMs);
        delayMs *= 2;
    }
    return res;
}

EmbedResult EmbedClient::EmbedMedia(const std::vector<uint8_t>& data, const char* mime, const std::string& text) const {
    json content = json::array();
    if (!text.empty()) content.push_back({ {"type", "text"}, {"text", text} });
    // Audio rides in the same image_url part as images (verified: data:audio/wav works).
    content.push_back({ {"type", "image_url"},
                        {"image_url", { {"url", "data:" + std::string(mime) + ";base64," + Base64(data.data(), data.size())} }} });
    json body = { {"model", model}, {"dimensions", dims},
                  {"input", json::array({ { {"role", "user"}, {"content", content} } })} };
    return Call(body.dump());
}

EmbedResult EmbedClient::EmbedText(const std::string& text) const {
    json body = { {"model", model}, {"dimensions", dims}, {"input", text} };
    return Call(body.dump());
}

bool EmbedClient::FetchPrices(const std::string& apiKey, const std::string& model,
                              Prices& prices, std::string& error) {
    prices = {};
    // Embedding models are listed separately from chat models. Image inputs are
    // billed at pricing.image per token (258 tokens per image for Gemini) and
    // audio at pricing.audio (25 tokens per second); fall back to the prompt
    // price for models without separate media rates.
    HttpResponse r = HttpRequest(L"GET", L"/api/v1/embeddings/models", apiKey, "");
    if (!r.ok) { error = r.error; return false; }
    if (r.status != 200) { error = ApiErrorMessage(r.body, r.status); return false; }
    json j = json::parse(r.body, nullptr, false);
    if (!j.is_object() || !j.contains("data") || !j["data"].is_array()) { error = "unexpected models response"; return false; }
    auto toPrice = [](const json& p) -> double {
        if (p.is_string()) return atof(p.get<std::string>().c_str());
        if (p.is_number()) return p.get<double>();
        return 0.0;
    };
    for (const json& m : j["data"]) {
        if (!m.is_object() || m.value("id", "") != model) continue;
        if (m.contains("pricing") && m["pricing"].is_object()) {
            const json& p = m["pricing"];
            double prompt = toPrice(p.value("prompt", json()));
            prices.image = toPrice(p.value("image", json()));
            prices.audio = toPrice(p.value("audio", json()));
            if (prices.image <= 0) prices.image = prompt;
            if (prices.audio <= 0) prices.audio = prompt;
        }
        return true;
    }
    error = "model not found in OpenRouter's embedding models";
    return false;
}

bool EmbedClient::ValidateKey(const std::string& apiKey, std::string& error) {
    HttpResponse r = HttpRequest(L"GET", L"/api/v1/auth/key", apiKey, "");
    if (!r.ok) { error = r.error; return false; }
    if (r.status != 200) { error = ApiErrorMessage(r.body, r.status); return false; }
    return true;
}

// ---------------------------------------------------------------- key storage

std::string LoadApiKey(const std::wstring& file, bool& fromEnv) {
    fromEnv = false;
    std::vector<uint8_t> bytes;
    if (ReadFileBytes(file, bytes) && !bytes.empty()) {
        DATA_BLOB in{ (DWORD)bytes.size(), bytes.data() }, out{};
        if (CryptUnprotectData(&in, nullptr, nullptr, nullptr, nullptr, CRYPTPROTECT_UI_FORBIDDEN, &out)) {
            std::string key((const char*)out.pbData, out.cbData);
            LocalFree(out.pbData);
            if (!key.empty()) return key;
        }
    }
    wchar_t buf[1024];
    DWORD n = GetEnvironmentVariableW(L"OPENROUTER_API_KEY", buf, 1024);
    if (n > 0 && n < 1024) { fromEnv = true; return WideToUtf8(buf); }
    return {};
}

bool SaveApiKey(const std::wstring& file, const std::string& key) {
    std::error_code ec;
    if (key.empty()) { fs::remove(file, ec); return true; }
    fs::create_directories(fs::path(file).parent_path(), ec);
    DATA_BLOB in{ (DWORD)key.size(), (BYTE*)key.data() }, out{};
    if (!CryptProtectData(&in, L"OpenRouter API key", nullptr, nullptr, nullptr, CRYPTPROTECT_UI_FORBIDDEN, &out))
        return false;
    FILE* f = _wfopen(file.c_str(), L"wb");
    bool ok = f && fwrite(out.pbData, 1, out.cbData, f) == out.cbData;
    if (f) fclose(f);
    LocalFree(out.pbData);
    return ok;
}

// ---------------------------------------------------------------- indexer

static double NowSeconds() {
    return std::chrono::duration<double>(std::chrono::steady_clock::now().time_since_epoch()).count();
}

void EmbedIndexer::Start(EmbedIndex* idx, EmbedClient c, EmbedMode md, int n,
                         std::vector<EmbedJob> list, MediaFn fn, double maxCostUsd) {
    Join();
    index = idx; client = std::move(c); mode = md; mediaFn = std::move(fn); jobs = std::move(list);
    next = 0; done = 0; failed = 0; total = (int)jobs.size();
    cancel = false; aborted = false; budgetHit = false; abortReason.clear();
    { std::lock_guard<std::mutex> l(m); cost = 0; maxCost = maxCostUsd; errors.clear(); }
    startTime = NowSeconds();
    n = (std::max)(1, (std::min)(16, n));
    active = n;
    for (int i = 0; i < n; ++i) threads.emplace_back([this] { Worker(); });
}

void EmbedIndexer::Cancel() { cancel = true; }

void EmbedIndexer::Join() {
    for (auto& t : threads) if (t.joinable()) t.join();
    threads.clear();
}

void EmbedIndexer::Worker() {
    for (;;) {
        if (cancel) break;
        size_t i = next.fetch_add(1);
        if (i >= jobs.size()) break;
        const EmbedJob& job = jobs[i];

        auto fail = [&](const std::string& why) {
            ++failed;
            std::lock_guard<std::mutex> l(m);
            if (errors.size() < 20) errors.push_back(job.relPath + ": " + why);
        };

        std::vector<uint8_t> data;
        const char* mime = nullptr;
        if (!mediaFn(job, data, mime) || data.empty() || !mime) { fail("could not decode"); continue; }
        std::string text = mode == EmbedMode::Hybrid ? EmbedHybridText(job.type, job.relPath, job.name) : "";
        EmbedResult r = client.EmbedMedia(data, mime, text);
        if (!r.ok) {
            if (r.httpStatus == 401 || r.httpStatus == 403) {
                std::lock_guard<std::mutex> l(m);
                if (!aborted) { aborted = true; abortReason = r.error; }
                cancel = true;
            }
            fail(r.error);
            continue;
        }
        index->Append(job.relPath, job.mtime, job.size, r.vec.data());
        ++done;
        std::lock_guard<std::mutex> l(m);
        cost += r.cost;
        if (maxCost > 0 && cost >= maxCost && !budgetHit) { budgetHit = true; cancel = true; }
    }
    --active;
}
