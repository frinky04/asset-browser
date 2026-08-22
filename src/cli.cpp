// ab: headless command-line front end for scripts and coding agents.
//
// Conventions: data on stdout (JSON when piped or --json), progress and errors
// on stderr, exit codes 0 ok / 1 error / 2 usage / 3 semantic unavailable / 4 not found.

#include "assets.h"
#include "embed.h"
#include "paths.h"
#include "search.h"
#include "util.h"

#include <nlohmann/json.hpp>
#include "stb_image.h"
#include "stb_image_resize2.h"
#include "stb_image_write.h"
#include "audio.h"
#include "model.h"
#include "preview.h"
#include <io.h>
#include <filesystem>
#include <fstream>
#include <shellapi.h>
#include <algorithm>
#include <chrono>
#include <cstdio>
#include <cstring>
#include <set>
#include <string>
#include <thread>
#include <cmath>
#include <ctime>
#include <vector>

using json = nlohmann::json;
namespace fs = std::filesystem;

namespace {

enum Exit { ExitOk = 0, ExitError = 1, ExitUsage = 2, ExitNoSemantic = 3, ExitNotFound = 4 };

struct Options {
    std::string command;
    std::vector<std::string> positional;
    std::wstring root;
    bool json = false;
    bool quiet = false;
    bool semantic = false;
    std::string similar;       // asset id or relPath
    std::string type;          // tex|model|audio
    std::string pack;
    int limit = 10;
    int offset = 0;
    std::string sort = "relevance";
    // export
    std::wstring to;
    std::string name;
    std::string format;        // png | glb|gltf|fbx|obj|dae | wav
    int resize = 0;            // max side in px (textures)
    double rangeA = -1, rangeB = -1; // seconds (audio)
    bool loop = false;
    double crossfadeMs = 50;
    bool trimSilence = false;
    bool yes = false;
    bool manifest = true;
    // previews
    std::wstring sheet;        // search --sheet out.png
    std::wstring preview;      // inspect --preview out.png
    int tile = 256;
};

const char* kUsage = R"(ab - headless Asset Browser for scripts and coding agents

Usage:
  ab search <query...> [options]   rank assets in the library
  ab packs                         list top-level packs with counts
  ab status                        library root, counts, semantic index state
  ab inspect <id|relPath>          dimensions / triangles / duration for one asset
             [--preview out.png]   also write an image: fitted texture, waveform, or 6-view model sheet
  ab export  <id|relPath> --to <dir> [options]   copy or convert into a project
  ab --help | --version

Search options:
  --semantic          rank by meaning (needs an OpenRouter key and a built index)
  --similar <id>      rank by similarity to an indexed asset (no API call)
  --type tex|model|audio
  --pack <name>       restrict to one top-level folder
  --limit N           results per page (default 10)
  --offset N          skip the first N results
  --sort relevance|name|newest|largest
  --sheet <out.png>   write a numbered contact sheet of this page of results
  --tile N            contact sheet cell size in px (default 256)

Query syntax (same as the GUI search box):
  words            fuzzy match on file name and path    "exact phrase"
  -word            exclude                              type:tex|model|audio
  ext:png,jpg      pack:Kenney   path:folder            size:<2mb  age:<30d
  indexed:yes|no   (semantic index membership)

Export options:
  --to <dir>          destination folder (created if missing)
  --name <stem>       output file name without extension (default: asset name)
  --resize N          textures: scale so the longest side is N px (writes PNG)
  --format <ext>      models: glb|gltf|fbx|obj|dae variant to copy
                      textures: png (forces re-encode)   audio: wav (forces decode)
  --range A-B         audio: seconds to keep, e.g. 0.5-2.25
  --loop              audio: crossfade the end into the start for seamless looping
  --crossfade MS      audio: loop crossfade length (default 50)
  --trim-silence      audio: drop leading/trailing silence below -50 dBFS
  --yes               overwrite existing output files
  --no-manifest       don't append to <dir>/assets-used.json

Global options:
  --root <dir>   asset library (default: the folder chosen in the GUI)
  --json         JSON output (automatic when stdout is not a terminal)
  --quiet        no progress on stderr

Examples:
  ab search "mossy stone" --type tex --limit 5
  ab search --semantic "weathered wooden door creak" --type audio
  ab search --similar 3f2a9c1e0b7d4e55 --limit 8
  ab status --json
  ab search "rock" --type model --limit 12 --sheet candidates.png   # then view the PNG
  ab inspect 61d3f6f2393f9544 --preview look.png
  ab export 61d3f6f2393f9544 --to ./assets/textures --resize 512
  ab export 622c944aa6f7aeba --to ./assets/sfx --range 0-1.5 --trim-silence

JSON results carry a stable "id" (hash of the library-relative path) plus the
absolute "path"; pass the id to --similar.
)";

int Fail(const std::string& msg, int code = ExitError) {
    fprintf(stderr, "error: %s\n", msg.c_str());
    return code;
}

std::string IdFor(const AssetEntry& e) {
    char buf[24];
    snprintf(buf, sizeof(buf), "%016llx", (unsigned long long)Fnv1a64(e.relPath.data(), e.relPath.size()));
    return buf;
}

const char* TypeName(AssetType t) {
    switch (t) { case AssetType::Texture: return "texture"; case AssetType::Model: return "model"; default: return "audio"; }
}

bool ParseArgs(int argc, wchar_t** argv, Options& o, std::string& err) {
    auto need = [&](int& i, std::string& out) {
        if (i + 1 >= argc) { err = "missing value for " + WideToUtf8(argv[i]); return false; }
        out = WideToUtf8(argv[++i]);
        return true;
    };
    for (int i = 1; i < argc; ++i) {
        std::string a = WideToUtf8(argv[i]);
        std::string v;
        if (a == "--help" || a == "-h") { o.command = "help"; return true; }
        if (a == "--version") { o.command = "version"; return true; }
        if (a == "--json") o.json = true;
        else if (a == "--quiet" || a == "-q") o.quiet = true;
        else if (a == "--semantic") o.semantic = true;
        else if (a == "--root") { if (!need(i, v)) return false; o.root = Utf8ToWide(v); }
        else if (a == "--similar") { if (!need(i, o.similar)) return false; }
        else if (a == "--type") { if (!need(i, o.type)) return false; }
        else if (a == "--pack") { if (!need(i, o.pack)) return false; }
        else if (a == "--sort") { if (!need(i, o.sort)) return false; }
        else if (a == "--sheet") { if (!need(i, v)) return false; o.sheet = Utf8ToWide(v); }
        else if (a == "--preview") { if (!need(i, v)) return false; o.preview = Utf8ToWide(v); }
        else if (a == "--tile") { if (!need(i, v)) return false; o.tile = atoi(v.c_str()); if (o.tile < 64 || o.tile > 1024) { err = "--tile must be 64..1024"; return false; } }
        else if (a == "--to") { if (!need(i, v)) return false; o.to = Utf8ToWide(v); }
        else if (a == "--name") { if (!need(i, o.name)) return false; }
        else if (a == "--format") { if (!need(i, o.format)) return false; o.format = ToLowerAscii(o.format); if (!o.format.empty() && o.format[0] == '.') o.format.erase(0, 1); }
        else if (a == "--resize") { if (!need(i, v)) return false; o.resize = atoi(v.c_str()); if (o.resize <= 0) { err = "--resize must be a positive pixel size"; return false; } }
        else if (a == "--range") {
            if (!need(i, v)) return false;
            size_t dash = v.find('-', 1);
            if (dash == std::string::npos || sscanf_s(v.c_str(), "%lf", &o.rangeA) != 1 ||
                sscanf_s(v.c_str() + dash + 1, "%lf", &o.rangeB) != 1 || o.rangeA < 0 || o.rangeB <= o.rangeA) {
                err = "--range must be START-END in seconds, e.g. 0.5-2.25"; return false;
            }
        }
        else if (a == "--crossfade") { if (!need(i, v)) return false; o.crossfadeMs = atof(v.c_str()); }
        else if (a == "--loop") o.loop = true;
        else if (a == "--trim-silence") o.trimSilence = true;
        else if (a == "--yes" || a == "-y") o.yes = true;
        else if (a == "--no-manifest") o.manifest = false;
        else if (a == "--limit" || a == "--offset") {
            if (!need(i, v)) return false;
            int n = atoi(v.c_str());
            if (n < 0 || (a == "--limit" && n == 0)) { err = a + " must be a positive number"; return false; }
            (a == "--limit" ? o.limit : o.offset) = n;
        }
        else if (a.rfind("--", 0) == 0) { err = "unknown option " + a; return false; }
        else if (o.command.empty()) o.command = a;
        else o.positional.push_back(a);
    }
    if (o.command.empty()) { err = "no command given"; return false; }
    return true;
}

// Scan the whole library synchronously, reporting progress on stderr.
bool ScanLibrary(AssetDatabase& db, const std::wstring& root, bool quiet, bool json) {
    db.StartScan(root);
    auto last = std::chrono::steady_clock::now();
    while (db.scanning) {
        std::this_thread::sleep_for(std::chrono::milliseconds(20));
        db.PumpScan();
        auto now = std::chrono::steady_clock::now();
        if (!quiet && now - last > std::chrono::milliseconds(500)) {
            last = now;
            if (json) fprintf(stderr, "{\"event\":\"scan\",\"files\":%zu}\n", db.entries.size());
            else fprintf(stderr, "scanning... %zu files\r", db.entries.size());
        }
    }
    db.PumpScan();
    db.StopScan();
    return true;
}

struct Semantic {
    EmbedIndex index;
    bool loaded = false;
    std::vector<std::string> excludes;
    std::string key;
    EmbedSettings settings;
};

void LoadSemantic(Semantic& s, const std::wstring& root) {
    std::wstring path = EmbedIndexPathFor(root);
    s.loaded = s.index.Load(path);
    s.excludes = LoadEmbedExcludes(path);
    s.settings.Load(EmbedSettingsFile());
    bool fromEnv = false;
    s.key = LoadApiKey(EmbedKeyFile(), fromEnv);
}

int FindByIdOrPath(const AssetDatabase& db, const std::string& ref) {
    std::string lower = ToLowerAscii(ref);
    for (char& c : lower) if (c == '\\') c = '/';
    for (size_t i = 0; i < db.entries.size(); ++i)
        if (IdFor(db.entries[i]) == lower || db.entries[i].relPathLower == lower) return (int)i;
    return -1;
}

json EntryJson(const AssetEntry& e, const std::wstring& root) {
    json j = {
        {"id", IdFor(e)},
        {"name", e.name},
        {"type", TypeName(e.type)},
        {"ext", e.ext.size() > 1 ? e.ext.substr(1) : e.ext},
        {"pack", e.pack},
        {"relPath", e.relPath},
        {"path", WideToUtf8(e.path)},
        {"size", e.size},
        {"mtime", e.mtime},
    };
    if (!e.variants.empty()) {
        json v = json::array();
        for (const auto& p : e.variants) v.push_back(WideToUtf8(p));
        j["variants"] = v;
    }
    return j;
}

int CmdSearch(const Options& o, AssetDatabase& db) {
    std::string queryText;
    for (const auto& p : o.positional) { if (!queryText.empty()) queryText += ' '; queryText += p; }

    FilterRequest req;
    req.query = ParseQueryText(queryText);
    bool typeSel[3] = {false, false, false};
    if (!o.type.empty()) {
        std::string t = ToLowerAscii(o.type);
        if (t == "tex" || t == "texture" || t == "textures") typeSel[0] = true;
        else if (t == "model" || t == "models") typeSel[1] = true;
        else if (t == "audio" || t == "sound" || t == "sounds") typeSel[2] = true;
        else { fprintf(stderr, "error: --type must be tex, model or audio\n"); return ExitUsage; }
        req.typeSel = typeSel;
    }
    std::set<std::string> packs;
    if (!o.pack.empty()) { packs.insert(o.pack); req.packs = &packs; }
    if (o.sort == "name") req.sortMode = 1;
    else if (o.sort == "newest") req.sortMode = 2;
    else if (o.sort == "largest") req.sortMode = 3;
    else if (o.sort != "relevance") { fprintf(stderr, "error: --sort must be relevance, name, newest or largest\n"); return ExitUsage; }

    bool wantsIndex = o.semantic || !o.similar.empty();
    for (const SearchTerm& t : req.query.terms) if (t.kind == SearchTermKind::Indexed) wantsIndex = true;

    Semantic sem;
    std::vector<float> queryVec;
    std::string rankedBy = "fuzzy";
    if (wantsIndex) {
        LoadSemantic(sem, db.root);
        if (sem.loaded) {
            req.index = &sem.index;
            req.excluded = [&](const AssetEntry& e) { return EmbedFolderListExcludes(sem.excludes, e); };
        }
    }
    if (!o.similar.empty()) {
        if (!sem.loaded) { fprintf(stderr, "error: no semantic index for this library. Build one in the GUI (Semantic > Settings & indexing).\n"); return ExitNoSemantic; }
        int idx = FindByIdOrPath(db, o.similar);
        if (idx < 0) { fprintf(stderr, "error: no asset with id or path \"%s\". Use ids from 'ab search --json'.\n", o.similar.c_str()); return ExitNotFound; }
        const float* v = sem.index.VectorFor(db.entries[idx]);
        if (!v) { fprintf(stderr, "error: \"%s\" is not in the semantic index; try a text --semantic query instead.\n", db.entries[idx].relPath.c_str()); return ExitNoSemantic; }
        queryVec.assign(v, v + sem.index.dims);
        rankedBy = "similar:" + IdFor(db.entries[idx]);
    } else if (o.semantic) {
        if (!sem.loaded) { fprintf(stderr, "error: no semantic index for this library. Build one in the GUI (Semantic > Settings & indexing), or drop --semantic.\n"); return ExitNoSemantic; }
        if (sem.key.empty()) { fprintf(stderr, "error: no OpenRouter key. Set OPENROUTER_API_KEY or paste one in the GUI, or use --similar <id> which needs no key.\n"); return ExitNoSemantic; }
        std::string text = SemanticTextFromQuery(req.query);
        if (text.empty()) { fprintf(stderr, "error: --semantic needs some descriptive words in the query.\n"); return ExitUsage; }
        EmbedClient client;
        client.apiKey = sem.key;
        client.model = sem.index.model;
        client.dims = sem.index.dims;
        if (!o.quiet) fprintf(stderr, "embedding query...\n");
        EmbedResult r = client.EmbedText(text);
        if (!r.ok) { fprintf(stderr, "error: embedding request failed (%d): %s\n", r.httpStatus, r.error.c_str()); return ExitError; }
        queryVec = std::move(r.vec);
        rankedBy = "semantic";
    }
    if (!queryVec.empty()) req.semanticQuery = &queryVec;

    FilterResult r = FilterAssets(db.entries, req);
    size_t total = r.filtered.size();
    size_t from = std::min((size_t)o.offset, total);
    size_t to = std::min(from + (size_t)o.limit, total);

    json sheetInfo;
    if (!o.sheet.empty() && to > from) {
        PreviewRenderer pr;
        std::vector<RgbaImage> tiles;
        int failed = 0;
        for (size_t n = from; n < to; ++n) {
            RgbaImage img; std::string perr;
            if (!o.quiet) fprintf(stderr, "rendering %zu/%zu\r", n - from + 1, to - from);
            if (!pr.Make(db.entries[r.filtered[n]], o.tile, false, img, perr)) ++failed;
            tiles.push_back(std::move(img));
        }
        int cols = std::min<int>((int)tiles.size(), std::max(1, 1024 / o.tile));
        RgbaImage sheet = ComposeContactSheet(tiles, o.tile, cols);
        fs::path outPath = fs::absolute(fs::path(o.sheet));
        std::error_code ec; fs::create_directories(outPath.parent_path(), ec);
        if (!WritePng(outPath.wstring(), sheet)) { fprintf(stderr, "error: cannot write %s\n", WideToUtf8(outPath.wstring()).c_str()); return ExitError; }
        sheetInfo = {{"path", WideToUtf8(outPath.wstring())}, {"cols", cols}, {"tile", o.tile},
                     {"numbering", "1-based, row-major; tile k = results[k-1]"}, {"failed", failed}};
        if (!o.quiet) fprintf(stderr, "                      \r");
    }

    if (o.json) {
        json out;
        out["query"] = queryText;
        out["rankedBy"] = rankedBy;
        out["total"] = total;
        out["offset"] = from;
        out["limit"] = o.limit;
        out["typeCounts"] = {{"texture", r.typeCounts[0]}, {"model", r.typeCounts[1]}, {"audio", r.typeCounts[2]}};
        json results = json::array();
        for (size_t n = from; n < to; ++n) {
            int i = r.filtered[n];
            json j = EntryJson(db.entries[i], db.root);
            if (!r.semanticScore.empty()) j["score"] = r.semanticScore[i];
            results.push_back(std::move(j));
        }
        out["results"] = std::move(results);
        if (!sheetInfo.is_null()) out["sheet"] = sheetInfo;
        printf("%s\n", out.dump(2).c_str());
    } else {
        if (total == 0) {
            printf("No results for \"%s\".\n", queryText.c_str());
            if (rankedBy == "fuzzy") printf("Try fewer or broader words, or --semantic for a description-based search.\n");
            return ExitOk;
        }
        std::string range = to < total ? ", showing " + std::to_string(from + 1) + "-" + std::to_string(to) : "";
        printf("%zu result%s (%s)%s\n", total, total == 1 ? "" : "s", rankedBy.c_str(), range.c_str());
        for (size_t n = from; n < to; ++n) {
            int i = r.filtered[n];
            const AssetEntry& e = db.entries[i];
            if (!r.semanticScore.empty()) printf("%.3f  ", r.semanticScore[i]);
            printf("%s  %-7s %s\n", IdFor(e).c_str(), TypeName(e.type), WideToUtf8(e.path).c_str());
        }
        if (to < total) printf("... use --offset %zu for more\n", to);
        if (!sheetInfo.is_null()) printf("contact sheet: %s (numbered 1-%zu)\n", sheetInfo["path"].get<std::string>().c_str(), to - from);
    }
    return ExitOk;
}

int CmdPacks(const Options& o, AssetDatabase& db) {
    if (o.json) {
        json out = json::array();
        for (const PackInfo& p : db.packs) out.push_back({{"name", p.name}, {"count", p.count}});
        printf("%s\n", out.dump(2).c_str());
    } else {
        for (const PackInfo& p : db.packs) printf("%7d  %s\n", p.count, p.name.c_str());
    }
    return ExitOk;
}

int CmdStatus(const Options& o, AssetDatabase& db) {
    Semantic sem;
    LoadSemantic(sem, db.root);
    int indexedByType[3] = {0, 0, 0};
    if (sem.loaded)
        for (const AssetEntry& e : db.entries)
            if (!EmbedFolderListExcludes(sem.excludes, e) && sem.index.HasCurrent(e)) ++indexedByType[(int)e.type];
    if (o.json) {
        json out = {
            {"root", WideToUtf8(db.root)},
            {"assets", db.entries.size()},
            {"typeCounts", {{"texture", db.typeCounts[0]}, {"model", db.typeCounts[1]}, {"audio", db.typeCounts[2]}}},
            {"packs", db.packs.size()},
            {"semantic", {
                {"indexed", sem.loaded},
                {"hasKey", !sem.key.empty()},
                {"model", sem.loaded ? sem.index.model : sem.settings.model},
                {"dims", sem.loaded ? sem.index.dims : sem.settings.dims},
                {"vectors", sem.loaded ? sem.index.entries.size() : 0},
                {"currentByType", {{"texture", indexedByType[0]}, {"model", indexedByType[1]}, {"audio", indexedByType[2]}}},
                {"indexFile", WideToUtf8(EmbedIndexPathFor(db.root))},
            }},
        };
        printf("%s\n", out.dump(2).c_str());
    } else {
        printf("root:     %s\n", WideToUtf8(db.root).c_str());
        printf("assets:   %zu (%d textures, %d models, %d sounds) in %zu packs\n", db.entries.size(),
               db.typeCounts[0], db.typeCounts[1], db.typeCounts[2], db.packs.size());
        if (sem.loaded)
            printf("semantic: %zu vectors (%d textures, %d models, %d sounds), model %s, key %s\n",
                   sem.index.entries.size(), indexedByType[0], indexedByType[1], indexedByType[2],
                   sem.index.model.c_str(), sem.key.empty() ? "missing (--similar still works)" : "present");
        else
            printf("semantic: no index for this library (build one in the GUI)\n");
    }
    return ExitOk;
}

// ---------------------------------------------------------------- inspect

json TextureInfo(const AssetEntry& e, std::string& err) {
    std::vector<uint8_t> bytes;
    if (!ReadFileBytes(e.path, bytes) || bytes.empty()) { err = "cannot read file"; return json::object(); }
    int w = 0, h = 0, comp = 0;
    if (!stbi_info_from_memory(bytes.data(), (int)bytes.size(), &w, &h, &comp)) { err = stbi_failure_reason(); return json::object(); }
    bool pow2 = w > 0 && h > 0 && !(w & (w - 1)) && !(h & (h - 1));
    return {{"width", w}, {"height", h}, {"channels", comp}, {"hasAlpha", comp == 2 || comp == 4}, {"powerOfTwo", pow2}};
}

bool ModelIsGltf(const std::wstring& path) {
    std::wstring lower = ToLowerAsciiW(path);
    return lower.ends_with(L".glb") || lower.ends_with(L".gltf");
}

std::string ExtOf(const std::wstring& path) { // "glb"
    std::string ext = WideToUtf8(ToLowerAsciiW(fs::path(path).extension().wstring()));
    return ext.empty() ? ext : ext.substr(1);
}

json ModelInfo(const AssetEntry& e, std::string& err) {
    json j = json::object();
    json formats = json::array();
    formats.push_back(e.ext.substr(1));
    for (const auto& v : e.variants) formats.push_back(ExtOf(v));
    j["formats"] = formats;
    if (e.ext == ".dae") { err = "no parser for .dae (file is still exportable)"; return j; }
    CpuModel m = ModelIsGltf(e.path) ? LoadGlbCpu(e.path) : LoadFbxObjCpu(e.path);
    if (!m.valid) { err = "failed to parse model"; return j; }
    j["triangles"] = m.indices.size() / 3;
    j["vertices"] = m.verts.size();
    j["primitives"] = m.prims.size();
    j["embeddedTextures"] = m.images.size();
    j["bounds"] = {{"min", {m.bmin[0], m.bmin[1], m.bmin[2]}}, {"max", {m.bmax[0], m.bmax[1], m.bmax[2]}},
                   {"size", {m.bmax[0] - m.bmin[0], m.bmax[1] - m.bmin[1], m.bmax[2] - m.bmin[2]}}};
    return j;
}

json AudioInfo(const AssetEntry& e, std::string& err) {
    PcmData pcm = DecodeAudioFile(e.path);
    if (!pcm.valid || pcm.frames == 0) { err = "failed to decode audio"; return json::object(); }
    uint64_t t0 = 0, t1 = pcm.frames;
    EmbedAudioWindow(pcm, t0, t1, 1e9, true);
    float peak = 0;
    for (float v : pcm.samples) peak = std::max(peak, std::fabs(v));
    return {
        {"seconds", (double)pcm.frames / pcm.sampleRate},
        {"sampleRate", pcm.sampleRate},
        {"channels", pcm.channels},
        {"trimmed", {{"start", (double)t0 / pcm.sampleRate}, {"end", (double)t1 / pcm.sampleRate},
                     {"seconds", (double)(t1 - t0) / pcm.sampleRate}}},
        {"peakDbfs", peak > 0 ? 20.0 * std::log10(peak) : -120.0},
    };
}

json InspectJson(const AssetEntry& e, const std::wstring& root) {
    json j = EntryJson(e, root);
    std::string err;
    json info = e.type == AssetType::Texture ? TextureInfo(e, err)
              : e.type == AssetType::Model   ? ModelInfo(e, err)
                                             : AudioInfo(e, err);
    if (!err.empty()) j["warning"] = err;
    for (auto& [k, v] : info.items()) j[k] = v;
    return j;
}

int ResolveRef(const AssetDatabase& db, const Options& o, int& index) {
    if (o.positional.empty()) { fprintf(stderr, "error: %s needs an asset id or library-relative path\n", o.command.c_str()); return ExitUsage; }
    index = FindByIdOrPath(db, o.positional[0]);
    if (index < 0) { fprintf(stderr, "error: no asset with id or path \"%s\". Use ids from 'ab search --json'.\n", o.positional[0].c_str()); return ExitNotFound; }
    return ExitOk;
}

int CmdInspect(const Options& o, AssetDatabase& db) {
    int idx;
    if (int rc = ResolveRef(db, o, idx)) return rc;
    json j = InspectJson(db.entries[idx], db.root);
    if (!o.preview.empty()) {
        PreviewRenderer pr;
        RgbaImage img; std::string perr;
        fs::path outPath = fs::absolute(fs::path(o.preview));
        std::error_code ec; fs::create_directories(outPath.parent_path(), ec);
        if (!pr.Make(db.entries[idx], 512, /*sheet*/ true, img, perr) || !WritePng(outPath.wstring(), img))
            return Fail("preview failed: " + (perr.empty() ? "cannot write png" : perr));
        j["preview"] = {{"path", WideToUtf8(outPath.wstring())}, {"width", img.w}, {"height", img.h}};
        if (db.entries[idx].type == AssetType::Model) j["preview"]["layout"] = "3x2: hero, front, back, left, right, top";
    }
    if (o.json) { printf("%s\n", j.dump(2).c_str()); return ExitOk; }
    const AssetEntry& e = db.entries[idx];
    printf("%s  %s\n%s\n", IdFor(e).c_str(), TypeName(e.type), WideToUtf8(e.path).c_str());
    for (auto& [k, v] : j.items()) {
        if (k == "id" || k == "path" || k == "type" || k == "relPath" || k == "mtime") continue;
        std::string text = v.is_string() ? v.get<std::string>() : v.dump();
        printf("  %-17s %s\n", k.c_str(), text.c_str());
    }
    return ExitOk;
}

// ---------------------------------------------------------------- export

bool PrepareOutput(const fs::path& out, bool yes, std::string& err) {
    std::error_code ec;
    fs::create_directories(out.parent_path(), ec);
    if (fs::exists(out, ec) && !yes) { err = "output exists: " + WideToUtf8(out.wstring()) + " (pass --yes to overwrite)"; return false; }
    return true;
}

bool CopyTo(const fs::path& src, const fs::path& dst, bool yes, std::string& err) {
    if (!PrepareOutput(dst, yes, err)) return false;
    std::error_code ec;
    fs::copy_file(src, dst, fs::copy_options::overwrite_existing, ec);
    if (ec) { err = "copy failed: " + ec.message(); return false; }
    return true;
}

int ExportTexture(const Options& o, const AssetEntry& e, const fs::path& dir, const std::string& stem,
                  std::vector<fs::path>& outputs, json& transform) {
    if (!o.format.empty() && o.format != "png") return Fail("textures export as png (or unchanged without --format/--resize)", ExitUsage);
    std::string err;
    if (o.resize <= 0 && o.format != "png") {
        fs::path dst = dir / (Utf8ToWide(stem) + Utf8ToWide(e.ext));
        if (!CopyTo(e.path, dst, o.yes, err)) return Fail(err);
        outputs.push_back(dst);
        return ExitOk;
    }
    std::vector<uint8_t> bytes;
    int w = 0, h = 0, comp = 0;
    stbi_uc* px = ReadFileBytes(e.path, bytes) && !bytes.empty()
        ? stbi_load_from_memory(bytes.data(), (int)bytes.size(), &w, &h, &comp, 4) : nullptr;
    if (!px) return Fail("cannot decode " + WideToUtf8(e.path));
    int nw = w, nh = h;
    if (o.resize > 0 && (w > o.resize || h > o.resize)) {
        float sc = std::min((float)o.resize / w, (float)o.resize / h);
        nw = std::max(1, (int)(w * sc)); nh = std::max(1, (int)(h * sc));
    }
    std::vector<uint8_t> pixels;
    const uint8_t* src = px;
    if (nw != w || nh != h) {
        pixels.resize((size_t)nw * nh * 4);
        stbir_resize_uint8_srgb(px, w, h, w * 4, pixels.data(), nw, nh, nw * 4, STBIR_RGBA);
        src = pixels.data();
    }
    fs::path dst = dir / (Utf8ToWide(stem) + L".png");
    bool ok = PrepareOutput(dst, o.yes, err) &&
              stbi_write_png(WideToUtf8(dst.wstring()).c_str(), nw, nh, 4, src, nw * 4);
    stbi_image_free(px);
    if (!ok) return Fail(err.empty() ? "png write failed" : err);
    outputs.push_back(dst);
    transform = {{"from", {w, h}}, {"to", {nw, nh}}, {"format", "png"}};
    return ExitOk;
}

int ExportModel(const Options& o, const AssetEntry& e, const fs::path& dir, const std::string& stem,
                std::vector<fs::path>& outputs, json& transform) {
    std::string err;
    std::wstring chosen = e.path;
    if (!o.format.empty() && e.ext != "." + o.format) {
        chosen.clear();
        std::wstring want = Utf8ToWide("." + o.format);
        for (const std::wstring& v : e.variants)
            if (ToLowerAsciiW(v).ends_with(want)) { chosen = v; break; }
        if (chosen.empty()) {
            std::string have = e.ext.substr(1);
            for (const auto& v : e.variants) have += ", " + ExtOf(v);
            return Fail("no ." + o.format + " version of this model; available: " + have, ExitNotFound);
        }
        transform["format"] = o.format;
    }
    fs::path src(chosen);
    std::wstring ext = ToLowerAsciiW(src.extension().wstring());
    fs::path dst = dir / (Utf8ToWide(stem) + ext);
    if (!CopyTo(src, dst, o.yes, err)) return Fail(err);
    outputs.push_back(dst);

    // Companions: .mtl for OBJ; external buffers/images for .gltf (same relative paths).
    std::error_code ec;
    if (ext == L".obj") {
        fs::path mtl = src; mtl.replace_extension(L".mtl");
        fs::path mdst = dst; mdst.replace_extension(L".mtl");
        if (fs::exists(mtl, ec) && CopyTo(mtl, mdst, true, err)) outputs.push_back(mdst);
    } else if (ext == L".gltf") {
        std::ifstream in(src, std::ios::binary);
        json g = json::parse(in, nullptr, false);
        for (const char* key : {"buffers", "images"}) {
            if (!g.is_object() || !g.contains(key)) continue;
            for (const auto& item : g[key]) {
                if (!item.contains("uri")) continue;
                std::string uri = item["uri"].get<std::string>();
                if (uri.rfind("data:", 0) == 0) continue;
                fs::path rel = Utf8ToWide(uri);
                fs::path from = src.parent_path() / rel, to = dir / rel;
                if (fs::exists(from, ec) && CopyTo(from, to, true, err)) outputs.push_back(to);
            }
        }
    }
    return ExitOk;
}

int ExportAudio(const Options& o, const AssetEntry& e, const fs::path& dir, const std::string& stem,
                std::vector<fs::path>& outputs, json& transform) {
    std::string err;
    if (!o.format.empty() && o.format != "wav") return Fail("audio exports as wav (or unchanged without options)", ExitUsage);
    bool process = o.rangeA >= 0 || o.loop || o.trimSilence || o.format == "wav";
    if (!process) {
        fs::path dst = dir / (Utf8ToWide(stem) + Utf8ToWide(e.ext));
        if (!CopyTo(e.path, dst, o.yes, err)) return Fail(err);
        outputs.push_back(dst);
        return ExitOk;
    }
    PcmData pcm = DecodeAudioFile(e.path);
    if (!pcm.valid || pcm.frames == 0) return Fail("cannot decode " + WideToUtf8(e.path));
    uint64_t f0 = 0, f1 = pcm.frames;
    if (o.rangeA >= 0) {
        f0 = std::min(pcm.frames, (uint64_t)(o.rangeA * pcm.sampleRate));
        f1 = std::min(pcm.frames, (uint64_t)(o.rangeB * pcm.sampleRate));
        char msg[96];
        snprintf(msg, sizeof(msg), "--range is outside the clip (%.2f s long)", (double)pcm.frames / pcm.sampleRate);
        if (f1 <= f0) return Fail(msg, ExitUsage);
    }
    if (o.trimSilence) {
        // EmbedAudioWindow trims a whole clip, so trim a copy of the range.
        PcmData window;
        window.channels = pcm.channels; window.sampleRate = pcm.sampleRate; window.valid = true;
        window.frames = f1 - f0;
        window.samples.assign(pcm.samples.begin() + f0 * pcm.channels, pcm.samples.begin() + f1 * pcm.channels);
        uint64_t w0 = 0, w1 = window.frames;
        EmbedAudioWindow(window, w0, w1, 1e9, true);
        f1 = f0 + w1; f0 = f0 + w0;
    }
    fs::path dst = dir / (Utf8ToWide(stem) + L".wav");
    if (!PrepareOutput(dst, o.yes, err)) return Fail(err);
    bool ok;
    if (o.loop) {
        uint64_t cf = std::min((uint64_t)(o.crossfadeMs * pcm.sampleRate / 1000.0 + 0.5), (f1 - f0) / 2);
        PcmData loop = BuildSeamlessLoop(pcm, f0, f1, cf);
        ok = loop.valid && WriteWav16(dst.wstring(), loop, 0, loop.frames);
        transform["loopCrossfadeMs"] = o.crossfadeMs;
    } else {
        ok = WriteWav16(dst.wstring(), pcm, f0, f1);
    }
    if (!ok) return Fail("wav write failed");
    outputs.push_back(dst);
    transform["range"] = {(double)f0 / pcm.sampleRate, (double)f1 / pcm.sampleRate};
    transform["format"] = "wav";
    if (o.trimSilence) transform["trimSilence"] = true;
    return ExitOk;
}

void AppendManifest(const fs::path& dir, json entry, std::string& manifestPath) {
    fs::path mf = dir / L"assets-used.json";
    json list = json::array();
    {
        std::ifstream in(mf, std::ios::binary);
        if (in) { json j = json::parse(in, nullptr, false); if (j.is_array()) list = j; }
    }
    char stamp[32];
    time_t now = time(nullptr);
    struct tm t; gmtime_s(&t, &now);
    strftime(stamp, sizeof(stamp), "%Y-%m-%dT%H:%M:%SZ", &t);
    entry["exportedAt"] = stamp;
    // Re-exporting the same asset to the same files replaces the earlier record.
    list.erase(std::remove_if(list.begin(), list.end(), [&](const json& x) {
        return x.value("id", "") == entry["id"].get<std::string>() &&
               x.value("outputs", json::array()).dump() == entry["outputs"].dump(); }), list.end());
    list.push_back(entry);
    std::ofstream out(mf, std::ios::binary | std::ios::trunc);
    out << list.dump(2);
    manifestPath = WideToUtf8(mf.wstring());
}

int CmdExport(const Options& o, AssetDatabase& db) {
    int idx;
    if (int rc = ResolveRef(db, o, idx)) return rc;
    if (o.to.empty()) return Fail("export needs --to <dir>", ExitUsage);
    const AssetEntry& e = db.entries[idx];
    fs::path dir = fs::absolute(fs::path(o.to));
    std::string stem = o.name.empty() ? e.name : o.name;
    std::vector<fs::path> outputs;
    json transform = json::object();

    int rc = e.type == AssetType::Texture ? ExportTexture(o, e, dir, stem, outputs, transform)
           : e.type == AssetType::Model   ? ExportModel(o, e, dir, stem, outputs, transform)
                                          : ExportAudio(o, e, dir, stem, outputs, transform);
    if (rc) return rc;

    json result = {{"id", IdFor(e)}, {"name", e.name}, {"type", TypeName(e.type)},
                   {"source", WideToUtf8(e.path)}, {"relPath", e.relPath}, {"pack", e.pack}};
    json outs = json::array();
    for (const auto& p : outputs) outs.push_back(WideToUtf8(p.wstring()));
    result["outputs"] = outs;
    if (!transform.empty()) result["transform"] = transform;
    if (o.manifest) {
        std::string mf;
        AppendManifest(dir, result, mf);
        result["manifest"] = mf;
    }

    if (o.json) printf("%s\n", result.dump(2).c_str());
    else for (const auto& p : outputs) printf("%s\n", WideToUtf8(p.wstring()).c_str());
    return ExitOk;
}

} // namespace

int wmain(int argc, wchar_t** argv) {
    SetConsoleOutputCP(CP_UTF8);
    Options o;
    std::string err;
    if (!ParseArgs(argc, argv, o, err)) {
        fprintf(stderr, "error: %s\n\n%s", err.c_str(), kUsage);
        return ExitUsage;
    }
    if (o.command == "help") { fputs(kUsage, stdout); return ExitOk; }
    if (o.command == "version") { printf("ab %s\n", ASSET_BROWSER_VERSION); return ExitOk; }
    if (!_isatty(_fileno(stdout))) o.json = true;

    static const char* kCommands[] = {"search", "packs", "status", "inspect", "export"};
    if (std::find(std::begin(kCommands), std::end(kCommands), o.command) == std::end(kCommands)) {
        fprintf(stderr, "error: unknown command \"%s\"\n\n%s", o.command.c_str(), kUsage);
        return ExitUsage;
    }

    std::wstring root = o.root.empty() ? LoadSavedAssetRoot() : o.root;
    if (root.empty()) {
        fprintf(stderr, "error: no asset library chosen. Pass --root <dir> or pick a folder in the GUI once.\n");
        return ExitUsage;
    }
    DWORD attrs = GetFileAttributesW(root.c_str());
    if (attrs == INVALID_FILE_ATTRIBUTES || !(attrs & FILE_ATTRIBUTE_DIRECTORY)) {
        fprintf(stderr, "error: asset root is not a directory: %s\n", WideToUtf8(root).c_str());
        return ExitError;
    }

    AssetDatabase db;
    ScanLibrary(db, root, o.quiet, o.json);
    if (!o.quiet && !o.json) fprintf(stderr, "                                   \r");

    if (o.command == "search") return CmdSearch(o, db);
    if (o.command == "packs") return CmdPacks(o, db);
    if (o.command == "inspect") return CmdInspect(o, db);
    if (o.command == "export") return CmdExport(o, db);
    return CmdStatus(o, db);
}
