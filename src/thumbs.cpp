#include "thumbs.h"
#include "audio.h"
#include "util.h"
#include <filesystem>
#include <cstdio>

#include "stb_image.h"
#include "stb_image_resize2.h"

namespace fs = std::filesystem;

namespace {

#pragma pack(push, 1)
struct CacheHeader {
    uint32_t magic;   // 'ABT1'
    uint16_t w, h;
    uint64_t mtime;
    uint64_t size;
};
#pragma pack(pop)
// Bumped ABT1 -> ABT2 when thumb styling changed (waveform colors, bake background)
// so stale thumbnails regenerate lazily.
constexpr uint32_t kMagic = 0x32544241; // "ABT2"

// Decode an image file and fit it into kThumbSize, preserving aspect.
bool MakeTextureThumb(const std::wstring& path, int& outW, int& outH, std::vector<uint8_t>& outRgba) {
    std::vector<uint8_t> bytes;
    if (!ReadFileBytes(path, bytes) || bytes.empty()) return false;
    int w = 0, h = 0, comp = 0;
    stbi_uc* pixels = stbi_load_from_memory(bytes.data(), (int)bytes.size(), &w, &h, &comp, 4);
    if (!pixels) return false;

    int maxSide = Thumbs::kThumbSize;
    if (w <= maxSide && h <= maxSide) {
        outW = w; outH = h;
        outRgba.assign(pixels, pixels + (size_t)w * h * 4);
        stbi_image_free(pixels);
        return true;
    }
    float scale = (std::min)((float)maxSide / w, (float)maxSide / h);
    int nw = (std::max)(1, (int)(w * scale));
    int nh = (std::max)(1, (int)(h * scale));
    outRgba.resize((size_t)nw * nh * 4);
    stbir_resize_uint8_srgb(pixels, w, h, w * 4, outRgba.data(), nw, nh, nw * 4, STBIR_RGBA);
    stbi_image_free(pixels);
    outW = nw; outH = nh;
    return true;
}

} // namespace

void Thumbs::Init(Gfx* g, ModelRenderer* r, const std::wstring& dir) {
    gfx = g;
    renderer = r;
    cacheDir = dir;
    std::error_code ec;
    fs::create_directories(dir, ec);

    bakeRT.Create(gfx->device, kThumbSize, kThumbSize, /*withStaging*/ true);

    unsigned hw = std::thread::hardware_concurrency();
    unsigned n = (std::max)(2u, (std::min)(4u, hw > 2 ? hw - 2 : 2u));
    quit = false;
    for (unsigned i = 0; i < n; ++i)
        workers.emplace_back([this] { WorkerLoop(); });
}

void Thumbs::Shutdown() {
    {
        std::lock_guard<std::mutex> lock(jobMutex);
        quit = true;
        jobs.clear();
    }
    jobCv.notify_all();
    for (auto& t : workers)
        if (t.joinable()) t.join();
    workers.clear();
    for (auto& t : thumbs) SafeRelease(t.srv);
    thumbs.clear();
    bakeRT.Release();
    { std::lock_guard<std::mutex> l(doneMutex); done.clear(); }
    { std::lock_guard<std::mutex> l(bakeMutex); bakes.clear(); }
}

void Thumbs::EnsureCount(size_t n) {
    if (thumbs.size() < n) thumbs.resize(n);
}

std::wstring Thumbs::CachePathFor(const std::wstring& assetPath) const {
    std::string utf8 = WideToUtf8(assetPath);
    uint64_t h = Fnv1a64(utf8.data(), utf8.size());
    wchar_t name[32];
    swprintf_s(name, L"%016llx.abt", (unsigned long long)h);
    return cacheDir + L"\\" + name;
}

bool Thumbs::LoadCacheFile(const std::wstring& file, uint64_t mtime, uint64_t size,
                           int& w, int& h, std::vector<uint8_t>& rgba) {
    std::vector<uint8_t> bytes;
    if (!ReadFileBytes(file, bytes) || bytes.size() < sizeof(CacheHeader)) return false;
    CacheHeader hdr;
    memcpy(&hdr, bytes.data(), sizeof(hdr));
    if (hdr.magic != kMagic || hdr.mtime != mtime || hdr.size != size) return false;
    size_t need = sizeof(CacheHeader) + (size_t)hdr.w * hdr.h * 4;
    if (bytes.size() < need || hdr.w == 0 || hdr.h == 0) return false;
    w = hdr.w; h = hdr.h;
    rgba.assign(bytes.begin() + sizeof(CacheHeader), bytes.begin() + need);
    return true;
}

void Thumbs::SaveCacheFile(const std::wstring& file, uint64_t mtime, uint64_t size,
                           int w, int h, const uint8_t* rgba) {
    FILE* f = _wfopen(file.c_str(), L"wb");
    if (!f) return;
    CacheHeader hdr{ kMagic, (uint16_t)w, (uint16_t)h, mtime, size };
    fwrite(&hdr, sizeof(hdr), 1, f);
    fwrite(rgba, 1, (size_t)w * h * 4, f);
    fclose(f);
}

void Thumbs::Request(int index, const AssetEntry& entry) {
    if (index < 0 || index >= (int)thumbs.size()) return;
    Thumb& t = thumbs[index];
    if (t.state != ThumbState::None) return;

    // Models in formats we can't render (e.g. DAE) get a generic icon immediately.
    bool renderable = entry.ext == ".glb" || entry.ext == ".gltf" ||
                      entry.ext == ".fbx" || entry.ext == ".obj";
    if (entry.type == AssetType::Model && !renderable) {
        t.state = ThumbState::Icon;
        return;
    }

    t.state = ThumbState::Pending;
    Job job;
    job.index = index;
    job.type = entry.type;
    job.ext = entry.ext;
    job.path = entry.path;
    job.mtime = entry.mtime;
    job.size = entry.size;
    job.generation = generation.load();
    {
        std::lock_guard<std::mutex> lock(jobMutex);
        if (jobs.size() >= kMaxQueuedJobs) {
            int staleIndex = jobs.front().index;
            jobs.pop_front();
            if (staleIndex >= 0 && staleIndex < (int)thumbs.size() &&
                thumbs[staleIndex].state == ThumbState::Pending)
                thumbs[staleIndex].state = ThumbState::None;
        }
        jobs.push_back(std::move(job));
    }
    jobCv.notify_one();
}

void Thumbs::FlushPending() {
    ++generation;
    {
        std::lock_guard<std::mutex> lock(jobMutex);
        jobs.clear();
    }
    {
        std::lock_guard<std::mutex> lock(doneMutex);
        done.clear();
    }
    {
        std::lock_guard<std::mutex> lock(bakeMutex);
        bakes.clear();
    }
    for (Thumb& thumb : thumbs) {
        if (thumb.state == ThumbState::Pending)
            thumb.state = ThumbState::None;
    }
}

int Thumbs::PendingJobs() {
    std::lock_guard<std::mutex> lock(jobMutex);
    return (int)jobs.size();
}

void Thumbs::WorkerLoop() {
    for (;;) {
        Job job;
        {
            std::unique_lock<std::mutex> lock(jobMutex);
            jobCv.wait(lock, [this] { return quit || !jobs.empty(); });
            if (quit) return;
            job = std::move(jobs.back()); // LIFO: most recently requested = most likely visible
            jobs.pop_back();
        }

        std::wstring cachePath = CachePathFor(job.path);
        Done d;
        d.index = job.index;
        d.generation = job.generation;

        // Cache hit covers every asset type, including baked GLB renders.
        if (LoadCacheFile(cachePath, job.mtime, job.size, d.w, d.h, d.rgba)) {
            if (job.generation != generation.load()) continue;
            std::lock_guard<std::mutex> lock(doneMutex);
            done.push_back(std::move(d));
            continue;
        }

        if (job.type == AssetType::Texture) {
            if (MakeTextureThumb(job.path, d.w, d.h, d.rgba))
                SaveCacheFile(cachePath, job.mtime, job.size, d.w, d.h, d.rgba.data());
            else
                d.failed = true;
        } else if (job.type == AssetType::Audio) {
            PcmData pcm = DecodeAudioFile(job.path);
            if (pcm.valid) {
                d.w = d.h = kThumbSize;
                d.rgba = RenderWaveformThumb(pcm, d.w, d.h);
                SaveCacheFile(cachePath, job.mtime, job.size, d.w, d.h, d.rgba.data());
            } else {
                d.failed = true;
            }
        } else { // Model: CPU parse here, GPU bake on main thread
            bool isGltf = (job.ext == ".glb" || job.ext == ".gltf");
            CpuModel model = isGltf ? LoadGlbCpu(job.path) : LoadFbxObjCpu(job.path);
            if (model.valid) {
                Bake b;
                b.index = job.index;
                b.model = std::move(model);
                b.cachePath = std::move(cachePath);
                b.mtime = job.mtime;
                b.size = job.size;
                b.generation = job.generation;
                if (job.generation != generation.load()) continue;
                std::lock_guard<std::mutex> lock(bakeMutex);
                bakes.push_back(std::move(b));
                continue;
            }
            d.failed = true;
        }

        if (job.generation != generation.load()) continue;
        std::lock_guard<std::mutex> lock(doneMutex);
        done.push_back(std::move(d));
    }
}

void Thumbs::Pump() {
    // 1) Upload completed CPU thumbnails (budgeted)
    for (int i = 0; i < 16; ++i) {
        Done d;
        {
            std::lock_guard<std::mutex> lock(doneMutex);
            if (done.empty()) break;
            d = std::move(done.back());
            done.pop_back();
        }
        if (d.generation != generation.load()) continue;
        if (d.index < 0 || d.index >= (int)thumbs.size()) continue;
        Thumb& t = thumbs[d.index];
        if (d.failed || d.rgba.empty()) {
            t.state = ThumbState::Failed;
            continue;
        }
        t.srv = gfx->CreateTextureSRV(d.rgba.data(), d.w, d.h, false);
        t.w = (uint16_t)d.w;
        t.h = (uint16_t)d.h;
        t.state = t.srv ? ThumbState::Ready : ThumbState::Failed;
    }

    // 2) Bake GLB thumbnails (budgeted: GPU + readback each)
    for (int i = 0; i < 2; ++i) {
        Bake b;
        {
            std::lock_guard<std::mutex> lock(bakeMutex);
            if (bakes.empty()) break;
            b = std::move(bakes.back());
            bakes.pop_back();
        }
        if (b.generation != generation.load()) continue;
        if (b.index < 0 || b.index >= (int)thumbs.size()) continue;
        Thumb& t = thumbs[b.index];

        GpuModel gpu;
        if (!gpu.Create(*gfx, b.model)) {
            t.state = ThumbState::Failed;
            continue;
        }
        OrbitCamera cam;
        cam.FrameModel(gpu.bmin, gpu.bmax);
        // Linear-space clear that lands on ~#1A1A1D in the sRGB target,
        // matching the tile background.
        float clear[4] = { 0.0106f, 0.0106f, 0.0116f, 1.0f };
        renderer->Render(*gfx, gpu, bakeRT, cam, clear);
        gpu.Release();

        std::vector<uint8_t> pixels;
        if (bakeRT.ReadPixels(gfx->ctx, pixels)) {
            SaveCacheFile(b.cachePath, b.mtime, b.size, bakeRT.w, bakeRT.h, pixels.data());
            t.srv = gfx->CreateTextureSRV(pixels.data(), bakeRT.w, bakeRT.h, false);
            t.w = (uint16_t)bakeRT.w;
            t.h = (uint16_t)bakeRT.h;
            t.state = t.srv ? ThumbState::Ready : ThumbState::Failed;
        } else {
            t.state = ThumbState::Failed;
        }
    }
}

void Thumbs::ClearCacheDisk() {
    std::error_code ec;
    for (auto& de : fs::directory_iterator(cacheDir, ec)) {
        if (de.path().extension() == L".abt")
            fs::remove(de.path(), ec);
    }
}
