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
}

void EmbedSettings::Save(const std::wstring& file) const {
    std::error_code ec;
    fs::create_directories(fs::path(file).parent_path(), ec);
    json j = { {"model", model}, {"mode", (int)mode}, {"dims", dims}, {"concurrency", concurrency} };
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

bool EmbedIndex::Load(const std::wstring& path) {
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
    std::lock_guard<std::mutex> lock(fileMutex);
    entries.clear(); vectors.clear(); pending.clear();
    model = mdl; mode = md; dims = d; file = path;
    std::error_code ec;
    fs::create_directories(fs::path(path).parent_path(), ec);
    FILE* f = _wfopen(path.c_str(), L"wb");
    if (!f) return;
    WriteHeader(f, model, mode, dims);
    fclose(f);
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

void EmbedIndex::Prune(const std::vector<AssetEntry>& db) {
    std::unordered_map<std::string, const AssetEntry*> live;
    for (const AssetEntry& e : db) live[e.relPath] = &e;
    size_t before = entries.size();
    for (auto it = entries.begin(); it != entries.end();)
        it = live.count(it->first) ? std::next(it) : entries.erase(it);
    if (entries.size() != before) Rewrite();
}

void EmbedIndex::Rewrite() {
    std::lock_guard<std::mutex> lock(fileMutex);
    std::vector<float> packed;
    packed.reserve(entries.size() * dims);
    FILE* f = _wfopen(file.c_str(), L"wb");
    if (!f) return;
    WriteHeader(f, model, mode, dims);
    for (auto& [rel, e] : entries) {
        const float* v = vectors.data() + e.slot * dims;
        WriteRecord(f, rel, e.mtime, e.size, v, dims);
        e.slot = packed.size() / dims;
        packed.insert(packed.end(), v, v + dims);
    }
    fclose(f);
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

bool EmbedIsIndexable(const AssetEntry& e) {
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

EmbedResult EmbedClient::EmbedImage(const std::vector<uint8_t>& jpeg, const std::string& text) const {
    json content = json::array();
    if (!text.empty()) content.push_back({ {"type", "text"}, {"text", text} });
    content.push_back({ {"type", "image_url"},
                        {"image_url", { {"url", "data:image/jpeg;base64," + Base64(jpeg.data(), jpeg.size())} }} });
    json body = { {"model", model}, {"dimensions", dims},
                  {"input", json::array({ { {"role", "user"}, {"content", content} } })} };
    return Call(body.dump());
}

EmbedResult EmbedClient::EmbedText(const std::string& text) const {
    json body = { {"model", model}, {"dimensions", dims}, {"input", text} };
    return Call(body.dump());
}

bool EmbedClient::FetchPricePerToken(const std::string& apiKey, const std::string& model,
                                     double& price, std::string& error) {
    price = 0;
    // Embedding models are listed separately from chat models. Image inputs are
    // billed at pricing.image per token (258 tokens per image for Gemini); fall
    // back to the prompt price for models without a separate image rate.
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
            price = toPrice(m["pricing"].value("image", json()));
            if (price <= 0) price = toPrice(m["pricing"].value("prompt", json()));
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
                         std::vector<EmbedJob> list, ThumbFn fn) {
    Join();
    index = idx; client = std::move(c); mode = md; thumbFn = std::move(fn); jobs = std::move(list);
    next = 0; done = 0; failed = 0; total = (int)jobs.size();
    cancel = false; aborted = false; abortReason.clear();
    { std::lock_guard<std::mutex> l(m); cost = 0; errors.clear(); }
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

        int w = 0, h = 0;
        std::vector<uint8_t> rgba, jpeg;
        if (!thumbFn(job, w, h, rgba) || !EncodeJpeg(rgba.data(), w, h, jpeg)) { fail("could not decode"); continue; }
        std::string text;
        if (mode == EmbedMode::Hybrid) {
            std::string words = EmbedFilenameWords(job.name);
            text = words.empty() ? "texture" : "texture: " + words;
        }
        EmbedResult r = client.EmbedImage(jpeg, text);
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
    }
    --active;
}
