#pragma once
// BYOK embedding search (OpenRouter). Index textures as vectors, rank by cosine.
#include "assets.h"
#include "audio.h"
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
    double budgetUsd = 0;    // stop a run once its reported cost passes this (0 = no cap)
    int audioSeconds = 12;   // seconds of each clip that are embedded (4..30)
    bool audioTrimSilence = true; // drop leading/trailing silence before the window

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

    // Vectors are treated as user data: nothing here deletes them implicitly.
    // Every destructive step (Reset, Compact, Restore) backs the file up first
    // and rewrites atomically via a temp file.
    bool Load(const std::wstring& path);             // false = no usable file (tries the newest backup first)
    bool loadedFromBackup = false;                   // Load() fell back to a backup
    void Reset(const std::string& model, EmbedMode mode, int dims, const std::wstring& path);
    void Append(const std::string& relPath, uint64_t mtime, uint64_t size, const float* vec); // mem + disk
    int  CountMissing(const std::vector<AssetEntry>& db) const; // vectors whose files are gone
    void Compact(const std::vector<AssetEntry>& db);  // drop those vectors (explicit user action)
    bool IsStale(const AssetEntry& e) const;         // indexed, but mtime/size changed since
    void Adopt(const AssetEntry& e);                 // keep the old vector under the file's new mtime/size
    bool Matches(const EmbedSettings& s) const { return model == s.model && mode == s.mode && dims == s.dims; }
    bool HasCurrent(const AssetEntry& e) const;      // indexed and not stale
    const float* VectorFor(const AssetEntry& e) const;
    bool Empty() const { return entries.empty(); }
    bool Pump();                                     // main thread: merge vectors appended by workers

    std::wstring Backup() const;                     // copy file -> <file>.<timestamp>.bak; "" on failure
    static std::vector<std::wstring> ListBackups(const std::wstring& indexPath); // newest first
    bool Restore(const std::wstring& backup);        // backup current, copy backup over, reload

private:
    struct Pending { std::string relPath; uint64_t mtime, size; std::vector<float> vec; };
    bool LoadFile(const std::wstring& path);
    void Rewrite();
    std::mutex fileMutex;
    std::vector<Pending> pending;
};

// Textures worth indexing (colour/albedo maps, not normal/roughness/...), every
// sound, and models in a format the in-app renderer can draw.
bool EmbedIsIndexable(const AssetEntry& e);
bool EmbedModelRenderable(const std::string& ext);
std::string EmbedFilenameWords(const std::string& stem);  // "Wall_BrickRed_01" -> "wall brick red"
// Metadata sent alongside the media in hybrid mode ("texture: ..." / "sound: <folder> <name>").
std::string EmbedHybridText(AssetType type, const std::string& relPath, const std::string& name);

constexpr int    kEmbedTokensPerImage = 258;      // flat per image (measured)
constexpr int    kEmbedTokensPerAudioSecond = 25; // linear (measured)
constexpr double kEmbedAudioDefaultSeconds = 12.0; // default clip window
constexpr int    kEmbedModelView = 256;           // each view of a model contact sheet
constexpr int    kEmbedModelSheetCols = 3, kEmbedModelSheetRows = 2; // 6 views, one request

// Window of [f0, f1) as 16 kHz mono 16-bit WAV bytes; returns seconds encoded.
// trimSilence drops leading/trailing silence (below -50 dBFS) before the cap.
double EncodeWav16kMono(const PcmData& pcm, uint64_t f0, uint64_t f1, std::vector<uint8_t>& out,
                        double maxSeconds = kEmbedAudioDefaultSeconds, bool trimSilence = true);
// The [f0, f1) range EncodeWav16kMono would actually send (for previews / chips).
void EmbedAudioWindow(const PcmData& pcm, uint64_t& f0, uint64_t& f1, double maxSeconds, bool trimSilence);

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

    // media: JPEG ("image/jpeg") or WAV ("audio/wav"); text "" = media only
    EmbedResult EmbedMedia(const std::vector<uint8_t>& data, const char* mime, const std::string& text) const;
    EmbedResult EmbedText(const std::string& text) const;
    struct Prices { double image = 0, audio = 0; }; // per token
    static bool FetchPrices(const std::string& apiKey, const std::string& model, Prices& prices, std::string& error);
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
    AssetType type = AssetType::Texture;
    std::string relPath, name;
    std::wstring path;
    uint64_t mtime = 0, size = 0;
};

// Background indexing run over a list of assets.
class EmbedIndexer {
public:
    // Produce the upload bytes for a job (JPEG thumbnail or 16 kHz WAV) and its MIME type.
    using MediaFn = std::function<bool(const EmbedJob&, std::vector<uint8_t>& data, const char*& mime)>;

    void Start(EmbedIndex* index, EmbedClient client, EmbedMode mode, int threads,
               std::vector<EmbedJob> jobs, MediaFn mediaFn, double maxCostUsd = 0);
    void Cancel();  // asynchronous; workers exit after their current request
    void Join();
    bool Running() const { return active > 0; }   // workers still going
    bool HasThreads() const { return !threads.empty(); } // needs Join() once !Running()

    std::atomic<int> done{0}, failed{0}, total{0};
    std::atomic<bool> aborted{false};   // fatal (401 etc.)
    std::atomic<bool> budgetHit{false}; // stopped by the spending cap
    std::string abortReason;            // valid once !Running()
    double costSoFar() { std::lock_guard<std::mutex> l(m); return cost; }
    std::vector<std::string> RecentErrors() { std::lock_guard<std::mutex> l(m); return errors; }
    double startTime = 0;

private:
    void Worker();
    EmbedIndex* index = nullptr;
    EmbedClient client;
    EmbedMode mode = EmbedMode::Hybrid;
    MediaFn mediaFn;
    std::vector<EmbedJob> jobs;
    std::atomic<size_t> next{0};
    std::atomic<bool> cancel{false};
    std::atomic<int> active{0};
    std::vector<std::thread> threads;
    std::mutex m;
    double cost = 0;
    double maxCost = 0;
    std::vector<std::string> errors;
};
