#pragma once
// BYOK embedding search (OpenRouter). Index textures as vectors, rank by cosine.
#include "assets.h"
#include <atomic>
#include <condition_variable>
#include <deque>
#include <functional>
#include <mutex>
#include <string>
#include <thread>
#include <unordered_map>
#include <vector>

enum class EmbedMode : uint8_t { Hybrid = 0, ImageOnly = 1 };

struct EmbedSettings {
    std::string model = "google/gemini-embedding-2";
    EmbedMode mode = EmbedMode::Hybrid;
    int dims = 768;          // 768 or 3072
    int concurrency = 8;     // 1..16

    void Load(const std::wstring& file);
    void Save(const std::wstring& file) const;
};

// Per-library vector store. File: <embedDir>/<hash-of-root>.abe
struct EmbedIndex {
    struct Entry { uint64_t mtime = 0, size = 0; size_t slot = 0; };

    std::string model;
    EmbedMode mode = EmbedMode::Hybrid;
    int dims = 0;
    std::unordered_map<std::string, Entry> entries; // relPath -> entry
    std::vector<float> vectors;                     // slot * dims, L2-normalised
    std::wstring file;

    bool Load(const std::wstring& path);             // false = no usable file
    void Reset(const std::string& model, EmbedMode mode, int dims, const std::wstring& path);
    void Append(const std::string& relPath, uint64_t mtime, uint64_t size, const float* vec); // mem + disk
    void Prune(const std::vector<AssetEntry>& db);   // drop entries whose files are gone; rewrite
    bool Matches(const EmbedSettings& s) const { return model == s.model && mode == s.mode && dims == s.dims; }
    bool HasCurrent(const AssetEntry& e) const;      // indexed and not stale
    const float* VectorFor(const AssetEntry& e) const;
    bool Empty() const { return entries.empty(); }
    bool Pump();                                     // main thread: merge vectors appended by workers

private:
    struct Pending { std::string relPath; uint64_t mtime, size; std::vector<float> vec; };
    void Rewrite();
    std::mutex fileMutex;
    std::vector<Pending> pending;
};

// True for textures worth indexing (colour/albedo maps, not normal/roughness/...).
bool EmbedIsIndexable(const AssetEntry& e);
std::string EmbedFilenameWords(const std::string& stem);  // "Wall_BrickRed_01" -> "wall brick red"

struct EmbedResult {
    bool ok = false;
    int httpStatus = 0;
    std::string error;
    std::vector<float> vec;  // normalised, truncated to requested dims
    double cost = 0;
};

// Synchronous OpenRouter calls (run on worker threads).
struct EmbedClient {
    std::string apiKey;
    std::string model;
    int dims = 768;

    EmbedResult EmbedImage(const std::vector<uint8_t>& jpeg, const std::string& text /*"" = image only*/) const;
    EmbedResult EmbedText(const std::string& text) const;
    // price per prompt token for `model` (0 if unknown); blocks on network
    static bool FetchPricePerToken(const std::string& apiKey, const std::string& model,
                                   double& pricePerToken, std::string& error);
    static bool ValidateKey(const std::string& apiKey, std::string& error);

private:
    EmbedResult Call(const std::string& body) const;
};

// Encode the 256px thumbnail as JPEG for upload.
bool EncodeJpeg(const uint8_t* rgba, int w, int h, std::vector<uint8_t>& out, int quality = 85);

// API key storage: DPAPI file, env var OPENROUTER_API_KEY fallback.
std::string LoadApiKey(const std::wstring& file, bool& fromEnv);
bool SaveApiKey(const std::wstring& file, const std::string& key);  // empty key = delete file

struct EmbedJob {
    int index = -1;
    std::string relPath, name;
    std::wstring path;
    uint64_t mtime = 0, size = 0;
};

// Background indexing run over a list of assets.
class EmbedIndexer {
public:
    using ThumbFn = std::function<bool(const EmbedJob&, int& w, int& h, std::vector<uint8_t>& rgba)>;

    void Start(EmbedIndex* index, EmbedClient client, EmbedMode mode, int threads,
               std::vector<EmbedJob> jobs, ThumbFn thumbFn);
    void Cancel();  // asynchronous; workers exit after their current request
    void Join();
    bool Running() const { return active > 0; }   // workers still going
    bool HasThreads() const { return !threads.empty(); } // needs Join() once !Running()

    std::atomic<int> done{0}, failed{0}, total{0};
    std::atomic<bool> aborted{false};   // fatal (401 etc.)
    std::string abortReason;            // valid once !Running()
    double costSoFar() { std::lock_guard<std::mutex> l(m); return cost; }
    std::vector<std::string> RecentErrors() { std::lock_guard<std::mutex> l(m); return errors; }
    double startTime = 0;

private:
    void Worker();
    EmbedIndex* index = nullptr;
    EmbedClient client;
    EmbedMode mode = EmbedMode::Hybrid;
    ThumbFn thumbFn;
    std::vector<EmbedJob> jobs;
    std::atomic<size_t> next{0};
    std::atomic<bool> cancel{false};
    std::atomic<int> active{0};
    std::vector<std::thread> threads;
    std::mutex m;
    double cost = 0;
    std::vector<std::string> errors;
};
