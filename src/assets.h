#pragma once
#include <string>
#include <vector>
#include <atomic>
#include <mutex>
#include <thread>
#include <cstdint>

enum class AssetType : uint8_t { Texture, Model, Audio };

struct AssetEntry {
    std::wstring path;       // absolute path of canonical file
    std::string  relPath;    // utf8, relative to root, forward slashes (search target)
    std::string  name;       // filename stem, utf8 (display)
    std::string  relPathLower;
    std::string  nameLower;
    std::string  ext;        // lowercase canonical extension incl. dot, utf8 (".glb")
    std::string  pack;       // top-level folder name, utf8
    AssetType    type = AssetType::Texture;
    uint64_t     mtime = 0;
    uint64_t     size = 0;
    std::vector<std::wstring> variants; // other-format siblings (models only)
};

// Asset type for a lowercase extension with the dot (".wav"); false for files the library skips.
bool AssetTypeForExtension(const std::wstring& extLower, AssetType& type);

struct PackInfo { std::string name; int count = 0; };

// Entries are append-only; indices are stable for the lifetime of the app.
struct AssetDatabase {
    std::wstring root;
    std::vector<AssetEntry> entries;
    std::vector<PackInfo> packs;
    int typeCounts[3] = {0, 0, 0};

    std::atomic<bool> scanning{false};

    void StartScan(const std::wstring& rootDir);
    bool PumpScan();   // main thread; returns true if new entries arrived
    void StopScan();   // join

private:
    std::thread scanThread;
    std::mutex pendingMutex;
    std::vector<AssetEntry> pending;
    void ScanWorker();
};
