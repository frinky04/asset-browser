#pragma once
#include "gfx.h"
#include "model.h"
#include "assets.h"
#include <deque>
#include <mutex>
#include <thread>
#include <atomic>
#include <condition_variable>
#include <vector>

enum class ThumbState : uint8_t {
    None = 0,   // never requested
    Pending,    // queued / in flight
    Ready,      // srv available
    Icon,       // no image possible (e.g. FBX-only model) -> draw generic icon
    Failed,     // decode failed -> draw generic icon
};

struct Thumb {
    ThumbState state = ThumbState::None;
    ID3D11ShaderResourceView* srv = nullptr;
    uint16_t w = 0, h = 0;
};

class Thumbs {
public:
    static constexpr int kThumbSize = 256;

    void Init(Gfx* gfx, ModelRenderer* renderer, const std::wstring& cacheDir);
    void Shutdown();

    void EnsureCount(size_t n);
    void Request(int index, const AssetEntry& entry); // no-op unless state == None
    const Thumb& Get(int index) const { return thumbs[index]; }

    void Pump(); // main thread: drain completed uploads + bake queued GLB thumbs
    void ClearCacheDisk();

    int PendingJobs();

private:
    struct Job {
        int index = -1;
        AssetType type{};
        bool isGlb = false;
        std::wstring path;
        uint64_t mtime = 0, size = 0;
    };
    struct Done {
        int index = -1;
        int w = 0, h = 0;
        std::vector<uint8_t> rgba;
        bool failed = false;
    };
    struct Bake {
        int index = -1;
        CpuModel model;
        std::wstring cachePath;
        uint64_t mtime = 0, size = 0;
    };

    void WorkerLoop();
    std::wstring CachePathFor(const std::wstring& assetPath) const;
    static bool LoadCacheFile(const std::wstring& file, uint64_t mtime, uint64_t size,
                              int& w, int& h, std::vector<uint8_t>& rgba);
    static void SaveCacheFile(const std::wstring& file, uint64_t mtime, uint64_t size,
                              int w, int h, const uint8_t* rgba);

    Gfx* gfx = nullptr;
    ModelRenderer* renderer = nullptr;
    std::wstring cacheDir;

    std::vector<Thumb> thumbs;

    std::mutex jobMutex;
    std::condition_variable jobCv;
    std::deque<Job> jobs;
    std::vector<std::thread> workers;
    std::atomic<bool> quit{false};

    std::mutex doneMutex;
    std::deque<Done> done;

    std::mutex bakeMutex;
    std::deque<Bake> bakes;

    OffscreenTarget bakeRT;
};
