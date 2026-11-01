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
#include <atomic>
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

extern const char* const kSkillName;
extern const char* const kSkillMarkdown;

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
    std::string segment;       // audio: "N" (1-based) or "all"
    double normalizeLufs = 0;  // audio: target integrated loudness; 0 = off
    double peakDb = 1;         // audio: target sample peak in dBFS; > 0 = off
    double minSec = -1, maxSec = -1; // search: audio duration filter
    bool yes = false;
    bool manifest = true;
    bool install = false;      // skill --install
    bool project = false;      // skill --install --project: ./.claude and ./.agents instead of ~
    // previews
    std::wstring sheet;        // search --sheet out.png
    std::wstring preview;      // inspect --preview out.png
    int tile = 256;
    bool sharedTime = false;   // audio sheets: one time scale for every row
};

const char* kUsage = R"(ab - headless Asset Browser for scripts and coding agents

Usage:
  ab search <query...> [options]   rank assets in the library
  ab packs                         list top-level packs with counts
  ab status                        library root, counts, semantic index state
  ab inspect <ref>                 dimensions / triangles / audio measurements for one asset
             [--preview out.png]   also write an image: fitted texture, waveform + spectrogram, or 6-view model sheet
             [--loop]              audio: also measure and show the loop seam (end running into start)
  ab compare <ref> <ref>... [--sheet out.png]   measurements + one sheet for hand-picked assets
  ab export  <ref>... --to <dir> [options]   copy or convert into a project
  ab skill                         print the agent skill (SKILL.md)
  ab skill --install [dir]         install to ~/.claude/skills and ~/.agents/skills (or one dir)
  ab skill --install --project     install to ./.claude/skills and ./.agents/skills
  ab --help | --version

Search options:
  --semantic          rank by meaning (needs an OpenRouter key and a built index)
  --similar <id>      rank by similarity to an indexed asset (no API call)
  --type tex|model|audio
  --pack <name>       restrict to one top-level folder
  --limit N           results per page (default 10)
  --offset N          skip the first N results
  --sort relevance|name|newest|largest   (name search only; semantic results are by score)
  --sheet <out.png>   write a numbered contact sheet of this page of results
  --tile N            contact sheet cell size in px (default 256)
  --time fit|shared   audio sheets: each row fills the width (default) or all rows share one time scale
  --min-sec S         audio only: at least S seconds long
  --max-sec S         audio only: at most S seconds long

<ref> is an id, a library-relative path, or a path to any file on disk.
Audio sheets are stacked rows (all-audio pages): dB waveform over a log-frequency spectrogram,
with distinct sounds shaded and numbered. Keep audio sheets to about 6 rows per image.

Query syntax (same as the GUI search box):
  words            fuzzy match on file name and path    "exact phrase"
  -word            exclude                              type:tex|model|audio
  ext:png,jpg      pack:Kenney   path:folder            size:<2mb  age:<30d
  indexed:yes|no   (semantic index membership)

Export options:
  --to <dir>          destination folder (created if missing)
  --name <stem>       output file name without extension (default: asset name)
  --resize N          textures: downscale so the longest side is N px (never upscales; writes PNG)
  --format <ext>      models: glb|gltf|fbx|obj|dae variant to copy
                      textures: png (forces re-encode)   audio: wav (forces decode)
  --range A-B         audio: seconds to keep, e.g. 0.5-2.25
  --loop              audio: crossfade the end into the start for seamless looping
  --crossfade MS      audio: loop crossfade length (default 50)
  --trim-silence      audio: drop leading/trailing silence below -50 dBFS
  --segment N|all     audio: export distinct sound N (see 'ab inspect' segments), or each one
                      as <name>_01.wav, <name>_02.wav ...
  --normalize LUFS    audio: gain to this integrated loudness, e.g. -16. Peaks stay <= -1 dBFS, and
                      everything in one export lands at the same level (lower if a clip needs it)
  --peak DB           audio: gain so the sample peak is DB dBFS, e.g. -1
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
  ab search "footstep" --type audio --max-sec 3 --limit 6 --sheet steps.png
  ab compare 622c944aa6f7aeba 55261a2b477cf916 --sheet doors.png --time shared
  ab export 61d3f6f2393f9544 --to ./assets/textures --resize 512
  ab export 622c944aa6f7aeba --to ./assets/sfx --range 0-1.5 --trim-silence
  ab export 9b1c0d2e3f405162 --to ./assets/sfx --segment all --normalize -16
  ab inspect ./assets/sfx/door_loop.wav --preview seam.png --loop   # check your own output
  ab skill --install          # teach Claude Code, Codex, Cursor, Gemini CLI... when and how to use ab

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
        else if (a == "--segment") {
            if (!need(i, o.segment)) return false;
            o.segment = ToLowerAscii(o.segment);
            if (o.segment != "all" && atoi(o.segment.c_str()) < 1) { err = "--segment must be a number from 1 or 'all'"; return false; }
        }
        else if (a == "--normalize") {
            if (!need(i, v)) return false;
            o.normalizeLufs = atof(v.c_str());
            if (o.normalizeLufs >= 0 || o.normalizeLufs < -70) { err = "--normalize takes a negative LUFS target, e.g. -16"; return false; }
        }
        else if (a == "--peak") {
            if (!need(i, v)) return false;
            o.peakDb = atof(v.c_str());
            if (o.peakDb > 0 || o.peakDb < -60) { err = "--peak takes a dBFS target from -60 to 0, e.g. -1"; return false; }
        }
        else if (a == "--time") {
            if (!need(i, v)) return false;
            if (v != "fit" && v != "shared") { err = "--time must be fit or shared"; return false; }
            o.sharedTime = v == "shared";
        }
        else if (a == "--min-sec" || a == "--max-sec") {
            if (!need(i, v)) return false;
            double s = atof(v.c_str());
            if (s < 0) { err = a + " must be a number of seconds"; return false; }
            (a == "--min-sec" ? o.minSec : o.maxSec) = s;
        }
        else if (a == "--yes" || a == "-y") o.yes = true;
        else if (a == "--no-manifest") o.manifest = false;
        else if (a == "--install") o.install = true;
        else if (a == "--project") o.project = true;
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

// A file outside the library, passed by path: relPath holds its absolute path.
bool IsExternal(const AssetEntry& e) { return e.relPath.find(':') != std::string::npos; }

// Library asset by id or relPath, else any supported file on disk, appended to db.entries so
// the rest of the CLI treats it like a library asset. -1 not found, -2 unsupported file type.
int ResolveAny(AssetDatabase& db, const std::string& ref) {
    int i = FindByIdOrPath(db, ref);
    if (i >= 0) return i;
    std::error_code ec;
    fs::path p = fs::absolute(fs::path(Utf8ToWide(ref)), ec);
    if (ec || !fs::is_regular_file(p, ec)) return -1;
    AssetEntry e;
    if (!AssetTypeForExtension(ToLowerAsciiW(p.extension().wstring()), e.type)) return -2;
    e.path = p.wstring();
    e.ext = WideToUtf8(ToLowerAsciiW(p.extension().wstring()));
    e.name = WideToUtf8(p.stem().wstring());
    e.nameLower = ToLowerAscii(e.name);
    e.relPath = WideToUtf8(p.generic_wstring());
    e.relPathLower = ToLowerAscii(e.relPath);
    e.size = fs::file_size(p, ec);
    db.entries.push_back(std::move(e));
    return (int)db.entries.size() - 1;
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
    if (IsExternal(e)) { j["external"] = true; j.erase("pack"); j.erase("mtime"); }
    return j;
}

// ---------------------------------------------------------------- audio measurements

// Carriage-return progress only makes sense on a console; captured stderr would fill with it.
bool ShowProgress(bool quiet) { return !quiet && _isatty(_fileno(stderr)); }

double Rd(double v, int digits = 1) {
    double m = std::pow(10.0, digits);
    return std::round(v * m) / m + 0.0; // + 0.0 turns -0.0 into 0.0
}

std::string HzText(double hz) {
    char buf[32];
    if (hz >= 1000) snprintf(buf, sizeof(buf), "%.1f kHz", hz / 1000); else snprintf(buf, sizeof(buf), "%.0f Hz", hz);
    return buf;
}

// Header line for a strip: what an agent needs to rank candidates at a glance.
std::string AudioStatsLine(const AudioAnalysis& a) {
    char buf[256];
    size_t n = a.segments.size();
    snprintf(buf, sizeof(buf), "%.2fs  |  peak %.1f dBFS  |  %.1f LUFS  |  %s  |  %s  |  %zu sound%s",
             a.seconds, Rd(a.peakDb), Rd(a.lufsIntegrated), HzText(a.centroidHz).c_str(), SpectralCharacter(a.tonality),
             n, n == 1 ? "" : "s");
    std::string s = buf;
    if (a.clippedSamples) s += "  |  CLIPPED";
    return s;
}

json SegmentsJson(const AudioAnalysis& a) {
    json segs = json::array();
    for (size_t k = 0; k < a.segments.size(); ++k) {
        const AudioSegment& s = a.segments[k];
        segs.push_back({{"n", k + 1}, {"start", Rd(s.start, 3)}, {"end", Rd(s.end, 3)},
                        {"seconds", Rd(s.end - s.start, 3)}, {"peakDbfs", Rd(s.peakDb)}, {"rmsDbfs", Rd(s.rmsDb)}});
    }
    return segs;
}

// Compact numbers for search results; the full set (with segment times) comes from inspect.
json AudioSummaryJson(const AudioAnalysis& a) {
    return {{"seconds", Rd(a.seconds, 3)}, {"peakDbfs", Rd(a.peakDb)}, {"lufs", Rd(a.lufsIntegrated)},
            {"centroidHz", Rd(a.centroidHz, 0)}, {"character", SpectralCharacter(a.tonality)},
            {"sounds", a.segments.size()}, {"clipped", a.clippedSamples > 0}};
}

json AudioAnalysisJson(const AudioAnalysis& a, bool loop) {
    json loud = {{"integratedLufs", Rd(a.lufsIntegrated)}, {"momentaryMaxLufs", Rd(a.lufsMomentaryMax)},
                 {"rmsDbfs", Rd(a.rmsDb)}, {"crestDb", Rd(a.crestDb)}};
    if (a.hasShortTerm) loud["shortTermMaxLufs"] = Rd(a.lufsShortTermMax);
    if (a.seconds < 0.4) loud["note"] = "shorter than one 400 ms block: measured as a whole, so momentary = integrated";
    json j = {
        {"loudness", loud},
        {"noiseFloorDbfs", Rd(a.noiseFloorDb)},
        {"clippedSamples", a.clippedSamples},
        {"dcOffset", Rd(a.dcOffset, 4)},
        {"silence", {{"leading", Rd(a.leadingSilence, 3)}, {"trailing", Rd(a.trailingSilence, 3)}}},
        {"spectrum", {{"centroidHz", Rd(a.centroidHz, 0)}, {"dominantHz", Rd(a.dominantHz, 0)},
                      {"tonality", Rd(a.tonality, 2)}, {"character", SpectralCharacter(a.tonality)},
                      {"bands", {{"low", Rd(a.bandLow, 2)}, {"mid", Rd(a.bandMid, 2)}, {"high", Rd(a.bandHigh, 2)}}}}},
        {"segments", SegmentsJson(a)},
    };
    if (a.stereo) j["stereoCorrelation"] = Rd(a.stereoCorrelation, 2);
    if (loop) j["loopSeam"] = {{"levelJumpDb", Rd(a.loopLevelJumpDb)}, {"sampleJump", Rd(a.loopSampleJump, 3)}};
    return j;
}

std::string FolderOf(const AssetEntry& e) {
    size_t slash = e.relPath.find_last_of('/');
    return slash == std::string::npos ? std::string() : e.relPath.substr(0, slash);
}

struct DecodedClip {
    PcmData pcm;
    AudioAnalysis analysis;
};

AudioStripOptions StripFor(const AssetEntry& e, const DecodedClip& c, int index) {
    AudioStripOptions so;
    so.index = index;
    so.title = e.name + e.ext;
    so.subtitle = FolderOf(e);
    so.stats = AudioStatsLine(c.analysis);
    so.warn = c.analysis.clippedSamples > 0;
    so.segments = &c.analysis.segments;
    return so;
}

// A numbered sheet of `items`. All audio: stacked strips sized to stay readable at ~6 rows.
// Anything else: the square grid. `audio[k]` receives the measurements for audio item k.
bool WriteSheet(const Options& o, const std::vector<const AssetEntry*>& items, json& info, std::vector<json>& audio) {
    audio.assign(items.size(), json());
    fs::path outPath = fs::absolute(fs::path(o.sheet));
    std::error_code ec; fs::create_directories(outPath.parent_path(), ec);
    bool allAudio = std::all_of(items.begin(), items.end(), [](const AssetEntry* e) { return e->type == AssetType::Audio; });
    RgbaImage sheet;
    int failed = 0;
    if (allAudio) {
        std::vector<DecodedClip> clips(items.size());
        double span = 0;
        for (size_t k = 0; k < items.size(); ++k) {
            if (ShowProgress(o.quiet)) fprintf(stderr, "analysing %zu/%zu\r", k + 1, items.size());
            clips[k].pcm = DecodeAudioFile(items[k]->path);
            if (!clips[k].pcm.valid) { ++failed; continue; }
            clips[k].analysis = AnalyzeAudio(clips[k].pcm);
            audio[k] = AudioSummaryJson(clips[k].analysis);
            span = std::max(span, clips[k].analysis.seconds);
        }
        const int width = 1200, rowH = std::clamp(980 / (int)items.size(), 120, 260);
        std::vector<RgbaImage> rows;
        for (size_t k = 0; k < items.size(); ++k) {
            AudioStripOptions so = StripFor(*items[k], clips[k], (int)k + 1);
            so.width = width; so.height = rowH;
            if (o.sharedTime) so.spanSeconds = span;
            if (!clips[k].pcm.valid) so.stats = "cannot decode";
            rows.push_back(RenderAudioStrip(clips[k].pcm, so));
        }
        sheet = StackVertical(rows, 4);
        info = {{"path", WideToUtf8(outPath.wstring())}, {"layout", "rows"}, {"rows", items.size()},
                {"time", o.sharedTime ? "shared" : "fit"},
                {"numbering", "row k = results[k-1]; shaded, numbered bands = distinct sounds (export with --segment)"},
                {"failed", failed}};
    } else {
        PreviewRenderer pr;
        std::vector<RgbaImage> tiles;
        for (size_t k = 0; k < items.size(); ++k) {
            RgbaImage img; std::string perr;
            if (ShowProgress(o.quiet)) fprintf(stderr, "rendering %zu/%zu\r", k + 1, items.size());
            if (!pr.Make(*items[k], o.tile, false, img, perr)) ++failed;
            tiles.push_back(std::move(img));
        }
        int cols = std::min<int>((int)tiles.size(), std::max(1, 1024 / o.tile));
        sheet = ComposeContactSheet(tiles, o.tile, cols);
        info = {{"path", WideToUtf8(outPath.wstring())}, {"layout", "grid"}, {"cols", cols}, {"tile", o.tile},
                {"numbering", "1-based, row-major; tile k = results[k-1]"}, {"failed", failed}};
    }
    if (ShowProgress(o.quiet)) fprintf(stderr, "                      \r");
    if (!WritePng(outPath.wstring(), sheet)) { fprintf(stderr, "error: cannot write %s\n", WideToUtf8(outPath.wstring()).c_str()); return false; }
    info["width"] = sheet.w; info["height"] = sheet.h;
    return true;
}

// Durations for audio entries, read in parallel from file headers; -1 for anything else.
std::vector<double> AudioDurations(const AssetDatabase& db, const std::vector<int>& indices, bool quiet) {
    std::vector<double> out(indices.size(), -1.0);
    std::atomic<size_t> next{0}, done{0};
    unsigned n = std::max(1u, std::min(16u, std::thread::hardware_concurrency()));
    std::vector<std::thread> pool;
    for (unsigned t = 0; t < n; ++t)
        pool.emplace_back([&] {
            for (size_t k; (k = next++) < indices.size(); ++done) {
                const AssetEntry& e = db.entries[indices[k]];
                if (e.type == AssetType::Audio) out[k] = AudioDurationSeconds(e.path);
            }
        });
    while (ShowProgress(quiet) && done < indices.size()) {
        fprintf(stderr, "reading durations %zu/%zu\r", done.load(), indices.size());
        std::this_thread::sleep_for(std::chrono::milliseconds(200));
    }
    for (auto& t : pool) t.join();
    if (ShowProgress(quiet)) fprintf(stderr, "                                        \r");
    return out;
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

    if (req.sortMode != 0 && (o.semantic || !o.similar.empty()))
        return Fail("--sort replaces semantic ranking; drop --sort (results are already by relevance) or drop --semantic/--similar", ExitUsage);
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
    std::vector<double> seconds; // parallel to r.filtered when a duration filter ran
    if (o.minSec >= 0 || o.maxSec >= 0) {
        std::vector<double> all = AudioDurations(db, r.filtered, o.quiet);
        std::vector<int> kept;
        for (size_t k = 0; k < all.size(); ++k) {
            if (all[k] < 0 || (o.minSec >= 0 && all[k] < o.minSec) || (o.maxSec >= 0 && all[k] > o.maxSec)) continue;
            kept.push_back(r.filtered[k]);
            seconds.push_back(all[k]);
        }
        r.filtered = std::move(kept);
    }
    size_t total = r.filtered.size();
    size_t from = std::min((size_t)o.offset, total);
    size_t to = std::min(from + (size_t)o.limit, total);

    json sheetInfo;
    std::vector<json> audioInfo(to - from);
    if (!o.sheet.empty() && to > from) {
        std::vector<const AssetEntry*> items;
        for (size_t n = from; n < to; ++n) items.push_back(&db.entries[r.filtered[n]]);
        if (!WriteSheet(o, items, sheetInfo, audioInfo)) return ExitError;
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
            if (db.entries[i].type == AssetType::Texture) { // header-only read, cheap per page
                std::vector<uint8_t> bytes; int w = 0, h = 0, comp = 0;
                if (ReadFileBytes(db.entries[i].path, bytes) && stbi_info_from_memory(bytes.data(), (int)bytes.size(), &w, &h, &comp)) {
                    j["width"] = w; j["height"] = h;
                }
            } else if (db.entries[i].type == AssetType::Audio) {
                if (!audioInfo[n - from].is_null()) j["audio"] = audioInfo[n - from];
                else j["seconds"] = Rd(seconds.empty() ? AudioDurationSeconds(db.entries[i].path) : seconds[n], 3);
            }
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

json AudioInfo(const DecodedClip& c, bool loop, std::string& err) {
    const PcmData& pcm = c.pcm;
    if (!pcm.valid || pcm.frames == 0) { err = "failed to decode audio"; return json::object(); }
    uint64_t t0 = 0, t1 = pcm.frames;
    EmbedAudioWindow(pcm, t0, t1, 1e9, true);
    json j = {
        {"seconds", Rd(c.analysis.seconds, 3)},
        {"sampleRate", pcm.sampleRate},
        {"channels", pcm.channels},
        {"trimmed", {{"start", Rd((double)t0 / pcm.sampleRate, 3)}, {"end", Rd((double)t1 / pcm.sampleRate, 3)},
                     {"seconds", Rd((double)(t1 - t0) / pcm.sampleRate, 3)}}},
        {"peakDbfs", Rd(c.analysis.peakDb)},
    };
    json analysis = AudioAnalysisJson(c.analysis, loop); // keep alive: items() is a view
    for (auto& [k, v] : analysis.items()) j[k] = v;
    return j;
}

DecodedClip DecodeAndAnalyze(const AssetEntry& e) {
    DecodedClip c;
    c.pcm = DecodeAudioFile(e.path);
    if (c.pcm.valid && c.pcm.frames) c.analysis = AnalyzeAudio(c.pcm);
    return c;
}

// `clip` is required for audio entries.
json InspectJson(const AssetEntry& e, const std::wstring& root, const DecodedClip* clip, bool loop) {
    json j = EntryJson(e, root);
    std::string err;
    json info = e.type == AssetType::Texture ? TextureInfo(e, err)
              : e.type == AssetType::Model   ? ModelInfo(e, err)
                                             : AudioInfo(*clip, loop, err);
    if (!err.empty()) j["warning"] = err;
    for (auto& [k, v] : info.items()) j[k] = v;
    return j;
}

int ResolveAt(AssetDatabase& db, const std::string& ref, int& index) {
    index = ResolveAny(db, ref);
    if (index == -2) { fprintf(stderr, "error: \"%s\" is not a texture, model or sound file\n", ref.c_str()); return ExitUsage; }
    if (index < 0) { fprintf(stderr, "error: no asset with id or path \"%s\" and no such file. Use ids from 'ab search --json'.\n", ref.c_str()); return ExitNotFound; }
    return ExitOk;
}

int ResolveRef(AssetDatabase& db, const Options& o, int& index) {
    if (o.positional.empty()) { fprintf(stderr, "error: %s needs an asset id, library-relative path or file path\n", o.command.c_str()); return ExitUsage; }
    return ResolveAt(db, o.positional[0], index);
}

// Large strip with numbered segments; with `seam`, a second strip of the clip's last half
// second running into its first, which is what a looping player hears at the join.
RgbaImage AudioPreview(const AssetEntry& e, const DecodedClip& c, bool seam) {
    AudioStripOptions so = StripFor(e, c, 0);
    so.width = 1200; so.height = 520;
    std::vector<RgbaImage> parts{RenderAudioStrip(c.pcm, so)};
    if (seam) {
        uint64_t edge = std::min<uint64_t>(c.pcm.frames / 2, c.pcm.sampleRate / 2);
        PcmData joined = SlicePcm(c.pcm, c.pcm.frames - edge, c.pcm.frames);
        PcmData head = SlicePcm(c.pcm, 0, edge);
        joined.samples.insert(joined.samples.end(), head.samples.begin(), head.samples.end());
        joined.frames += head.frames;
        double edgeSec = (double)edge / c.pcm.sampleRate;
        AudioStripOptions ss;
        ss.width = 1200; ss.height = 220;
        char buf[160];
        snprintf(buf, sizeof(buf), "Loop seam: last %.2fs, then first %.2fs", edgeSec, edgeSec);
        ss.title = buf;
        snprintf(buf, sizeof(buf), "level jump %.1f dB  |  sample jump %.3f", c.analysis.loopLevelJumpDb, c.analysis.loopSampleJump);
        ss.stats = buf;
        ss.warn = c.analysis.loopLevelJumpDb > 3 || c.analysis.loopSampleJump > 0.05;
        ss.timeOffset = -edgeSec;
        ss.markerAt = 0;
        ss.markerLabel = "seam";
        parts.push_back(RenderAudioStrip(joined, ss));
    }
    return StackVertical(parts, 6);
}

void PrintInspectText(const json& j, const AssetEntry& e) {
    printf("%s  %s\n%s\n", IdFor(e).c_str(), TypeName(e.type), WideToUtf8(e.path).c_str());
    for (auto& [k, v] : j.items()) {
        if (k == "id" || k == "path" || k == "type" || k == "relPath" || k == "mtime" || k == "segments") continue;
        std::string text = v.is_string() ? v.get<std::string>() : v.dump();
        printf("  %-17s %s\n", k.c_str(), text.c_str());
    }
    if (j.contains("segments"))
        for (const json& s : j["segments"])
            printf("  sound %-11d %.3f-%.3f s  peak %.1f dBFS\n", s["n"].get<int>(), s["start"].get<double>(),
                   s["end"].get<double>(), s["peakDbfs"].get<double>());
}

int CmdInspect(const Options& o, AssetDatabase& db) {
    int idx;
    if (int rc = ResolveRef(db, o, idx)) return rc;
    const AssetEntry& e = db.entries[idx];
    DecodedClip clip;
    if (e.type == AssetType::Audio) clip = DecodeAndAnalyze(e);
    json j = InspectJson(e, db.root, &clip, o.loop);
    if (!o.preview.empty()) {
        RgbaImage img; std::string perr;
        fs::path outPath = fs::absolute(fs::path(o.preview));
        std::error_code ec; fs::create_directories(outPath.parent_path(), ec);
        if (e.type == AssetType::Audio) {
            if (!clip.pcm.valid) return Fail("preview failed: cannot decode audio");
            img = AudioPreview(e, clip, o.loop);
        } else {
            PreviewRenderer pr;
            if (!pr.Make(e, 512, /*sheet*/ true, img, perr)) return Fail("preview failed: " + perr);
        }
        if (!WritePng(outPath.wstring(), img)) return Fail("preview failed: cannot write png");
        j["preview"] = {{"path", WideToUtf8(outPath.wstring())}, {"width", img.w}, {"height", img.h}};
        if (e.type == AssetType::Model) j["preview"]["layout"] = "3x2: hero, front, back, left, right, top";
        if (e.type == AssetType::Audio)
            j["preview"]["layout"] = std::string("dB waveform over log-frequency spectrogram; shaded numbered bands = segments") +
                                     (o.loop ? "; bottom strip = loop seam" : "");
    }
    if (o.json) { printf("%s\n", j.dump(2).c_str()); return ExitOk; }
    PrintInspectText(j, e);
    return ExitOk;
}

// ---------------------------------------------------------------- compare

int CmdCompare(const Options& o, AssetDatabase& db) {
    if (o.positional.empty()) return Fail("compare needs one or more asset ids or file paths", ExitUsage);
    std::vector<int> idx;
    for (const std::string& ref : o.positional) {
        int i;
        if (int rc = ResolveAt(db, ref, i)) return rc;
        idx.push_back(i);
    }
    json items = json::array();
    for (int i : idx) {
        DecodedClip clip;
        if (db.entries[i].type == AssetType::Audio) clip = DecodeAndAnalyze(db.entries[i]);
        json j = InspectJson(db.entries[i], db.root, &clip, o.loop);
        if (db.entries[i].type == AssetType::Audio && clip.pcm.valid) j["summary"] = AudioStatsLine(clip.analysis);
        items.push_back(std::move(j));
    }
    json sheetInfo;
    if (!o.sheet.empty()) {
        std::vector<const AssetEntry*> entries;
        for (int i : idx) entries.push_back(&db.entries[i]);
        std::vector<json> unused;
        if (!WriteSheet(o, entries, sheetInfo, unused)) return ExitError;
    }
    if (o.json) {
        json out = {{"items", items}};
        if (!sheetInfo.is_null()) out["sheet"] = sheetInfo;
        printf("%s\n", out.dump(2).c_str());
        return ExitOk;
    }
    for (size_t k = 0; k < items.size(); ++k) {
        const json& j = items[k];
        printf("%zu  %s  %s\n", k + 1, j["id"].get<std::string>().c_str(), j["path"].get<std::string>().c_str());
        if (j.contains("summary")) printf("   %s\n", j["summary"].get<std::string>().c_str());
    }
    if (!sheetInfo.is_null()) printf("sheet: %s\n", sheetInfo["path"].get<std::string>().c_str());
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
    if (o.resize > 0 && nw < o.resize && nh < o.resize) {
        transform["note"] = "source smaller than --resize; not upscaled";
        fprintf(stderr, "warning: %dx%d source is smaller than %d; exported at original size (no upscaling)\n", w, h, o.resize);
    }
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

// With `reach` set this is a dry run for --normalize: nothing is written, and *reach is lowered
// to the loudest level every clip can get to without its peak passing -1 dBFS. The real run
// then normalizes every clip to `lufs`, so a set exported together matches.
int ExportAudio(const Options& o, const AssetEntry& e, const fs::path& dir, const std::string& stem,
                std::vector<fs::path>& outputs, json& transform, double lufs, double* reach = nullptr) {
    std::string err;
    if (!o.format.empty() && o.format != "wav") return Fail("audio exports as wav (or unchanged without options)", ExitUsage);
    bool gain = o.normalizeLufs < 0 || o.peakDb <= 0;
    if (o.normalizeLufs < 0 && o.peakDb <= 0) return Fail("use --normalize or --peak, not both", ExitUsage);
    if (!o.segment.empty() && o.rangeA >= 0) return Fail("use --segment or --range, not both", ExitUsage);
    if (o.segment == "all" && o.loop) return Fail("--loop needs one clip; pick --segment N", ExitUsage);
    bool process = o.rangeA >= 0 || o.loop || o.trimSilence || o.format == "wav" || !o.segment.empty() || gain;
    if (!process) {
        fs::path dst = dir / (Utf8ToWide(stem) + Utf8ToWide(e.ext));
        if (!CopyTo(e.path, dst, o.yes, err)) return Fail(err);
        outputs.push_back(dst);
        return ExitOk;
    }
    PcmData pcm = DecodeAudioFile(e.path);
    if (!pcm.valid || pcm.frames == 0) return Fail("cannot decode " + WideToUtf8(e.path));
    const double rate = pcm.sampleRate;

    struct Part { uint64_t f0, f1; std::wstring suffix; int segment = 0; };
    std::vector<Part> parts;
    if (!o.segment.empty()) {
        AudioAnalysis a = AnalyzeAudio(pcm);
        const size_t n = a.segments.size();
        if (n == 0) return Fail("no distinct sounds found (the clip is silent)");
        auto partFor = [&](size_t k, std::wstring suffix) {
            return Part{(uint64_t)(a.segments[k].start * rate), (uint64_t)(a.segments[k].end * rate), suffix, (int)k + 1};
        };
        if (o.segment == "all") {
            for (size_t k = 0; k < n; ++k) {
                wchar_t sfx[16]; swprintf(sfx, 16, n >= 100 ? L"_%03zu" : L"_%02zu", k + 1);
                parts.push_back(partFor(k, sfx));
            }
        } else {
            size_t k = (size_t)atoi(o.segment.c_str());
            if (k > n) {
                char msg[128]; snprintf(msg, sizeof(msg), "--segment %zu is past the last sound; this clip has %zu (see 'ab inspect')", k, n);
                return Fail(msg, ExitUsage);
            }
            parts.push_back(partFor(k - 1, L""));
        }
    } else {
        uint64_t f0 = 0, f1 = pcm.frames;
        if (o.rangeA >= 0) {
            f0 = std::min(pcm.frames, (uint64_t)(o.rangeA * rate));
            f1 = std::min(pcm.frames, (uint64_t)(o.rangeB * rate));
            char msg[96];
            snprintf(msg, sizeof(msg), "--range is outside the clip (%.2f s long)", pcm.frames / rate);
            if (f1 <= f0) return Fail(msg, ExitUsage);
        }
        parts.push_back({f0, f1, L""});
    }
    for (const Part& p : parts) // check every output first so a split never stops halfway
        if (!PrepareOutput(dir / (Utf8ToWide(stem) + p.suffix + L".wav"), o.yes, err)) return Fail(err);

    json partsJson = json::array();
    for (Part& p : parts) {
        if (o.trimSilence) {
            // EmbedAudioWindow trims a whole clip, so trim a copy of the range.
            PcmData window = SlicePcm(pcm, p.f0, p.f1);
            uint64_t w0 = 0, w1 = window.frames;
            EmbedAudioWindow(window, w0, w1, 1e9, true);
            p.f1 = p.f0 + w1; p.f0 = p.f0 + w0;
        }
        PcmData clip = SlicePcm(pcm, p.f0, p.f1);
        json info = {{"range", {Rd(p.f0 / rate, 3), Rd(p.f1 / rate, 3)}}};
        if (p.segment) info["segment"] = p.segment;
        if (gain) {
            AudioAnalysis a = AnalyzeAudio(clip);
            if (a.peakDb <= -119) return Fail("cannot normalize silence");
            if (reach) { // no limiter: keep 1 dB of peak headroom instead
                *reach = std::min(*reach, a.lufsIntegrated + (-1.0 - a.peakDb));
                continue;
            }
            double g = o.normalizeLufs < 0 ? lufs - a.lufsIntegrated : o.peakDb - a.peakDb;
            float lin = (float)std::pow(10.0, g / 20.0);
            for (float& s : clip.samples) s *= lin;
            info["gainDb"] = Rd(g, 2);
            info["lufs"] = Rd(a.lufsIntegrated + g);
            info["peakDbfs"] = Rd(a.peakDb + g);
        }
        fs::path dst = dir / (Utf8ToWide(stem) + p.suffix + L".wav");
        bool ok;
        if (o.loop) {
            uint64_t cf = std::min((uint64_t)(o.crossfadeMs * rate / 1000.0 + 0.5), clip.frames / 2);
            PcmData loop = BuildSeamlessLoop(clip, 0, clip.frames, cf);
            ok = loop.valid && WriteWav16(dst.wstring(), loop, 0, loop.frames);
        } else {
            ok = WriteWav16(dst.wstring(), clip, 0, clip.frames);
        }
        if (!ok) return Fail("wav write failed: " + WideToUtf8(dst.wstring()));
        outputs.push_back(dst);
        info["output"] = WideToUtf8(dst.wstring());
        partsJson.push_back(std::move(info));
    }
    if (reach) return ExitOk;
    if (partsJson.size() == 1) {
        for (auto& [k, v] : partsJson[0].items()) if (k != "output") transform[k] = v;
    } else {
        transform["parts"] = partsJson;
    }
    transform["format"] = "wav";
    if (o.loop) transform["loopCrossfadeMs"] = o.crossfadeMs;
    if (o.trimSilence) transform["trimSilence"] = true;
    if (o.normalizeLufs < 0) {
        transform["normalizeLufs"] = o.normalizeLufs;
        if (lufs < o.normalizeLufs - 0.05) {
            char note[160];
            snprintf(note, sizeof(note), "matched at %.1f LUFS: the loudest level every clip in this export reaches with its peak at or below -1 dBFS", lufs);
            transform["note"] = note;
        }
    }
    if (o.peakDb <= 0) transform["peakTargetDbfs"] = o.peakDb;
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

int ExportOne(const Options& o, const AssetEntry& e, const fs::path& dir, double lufs, json& result) {
    std::string stem = o.name.empty() ? e.name : o.name;
    std::vector<fs::path> outputs;
    json transform = json::object();
    int rc = e.type == AssetType::Texture ? ExportTexture(o, e, dir, stem, outputs, transform)
           : e.type == AssetType::Model   ? ExportModel(o, e, dir, stem, outputs, transform)
                                          : ExportAudio(o, e, dir, stem, outputs, transform, lufs);
    if (rc) return rc;
    result = {{"id", IdFor(e)}, {"name", e.name}, {"type", TypeName(e.type)},
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
    return ExitOk;
}

int CmdExport(const Options& o, AssetDatabase& db) {
    if (o.positional.empty()) return Fail("export needs an asset id, library-relative path or file path", ExitUsage);
    if (o.to.empty()) return Fail("export needs --to <dir>", ExitUsage);
    if (o.positional.size() > 1 && !o.name.empty()) return Fail("--name needs a single asset", ExitUsage);
    std::vector<int> idx;
    for (const std::string& ref : o.positional) {
        int i;
        if (int rc = ResolveAt(db, ref, i)) return rc;
        idx.push_back(i);
    }
    fs::path dir = fs::absolute(fs::path(o.to));

    // Everything exported together with --normalize lands at one loudness.
    double lufs = o.normalizeLufs;
    if (o.normalizeLufs < 0) {
        for (int i : idx) {
            const AssetEntry& e = db.entries[i];
            if (e.type != AssetType::Audio) continue;
            std::vector<fs::path> unusedOut;
            json unusedTransform;
            if (int rc = ExportAudio(o, e, dir, o.name.empty() ? e.name : o.name, unusedOut, unusedTransform, lufs, &lufs)) return rc;
        }
        if (lufs < o.normalizeLufs - 0.05)
            fprintf(stderr, "warning: normalized to %.1f LUFS, not %.1f: louder would push a peak past -1 dBFS. "
                            "Every clip in this export matches.\n", lufs, o.normalizeLufs);
    }

    json results = json::array();
    for (int i : idx) {
        json result;
        if (int rc = ExportOne(o, db.entries[i], dir, lufs, result)) return rc;
        results.push_back(std::move(result));
    }
    if (o.json) {
        if (results.size() == 1) printf("%s\n", results[0].dump(2).c_str());
        else printf("%s\n", json({{"exports", results}}).dump(2).c_str());
    } else {
        for (const json& r : results)
            for (const json& p : r["outputs"]) printf("%s\n", p.get<std::string>().c_str());
    }
    return ExitOk;
}

// ---------------------------------------------------------------- skill

std::string SkillText() {
    wchar_t exe[MAX_PATH * 2];
    DWORD n = GetModuleFileNameW(nullptr, exe, ARRAYSIZE(exe));
    std::string path = n ? WideToUtf8(std::wstring(exe, n)) : "ab";
    for (char& c : path) if (c == '\\') c = '/';
    std::string text = kSkillMarkdown;
    for (size_t at; (at = text.find("@AB_EXE@")) != std::string::npos;) text.replace(at, 8, path);
    return text;
}

int CmdSkill(const Options& o) {
    std::string text = SkillText();
    if (!o.install) { fputs(text.c_str(), stdout); return ExitOk; }
    // Targets: an explicit directory, or the Claude Code layout plus the
    // vendor-neutral .agents/skills layout (Codex, Cursor, Gemini CLI, Copilot, ...).
    std::vector<fs::path> roots;
    if (!o.positional.empty()) roots.push_back(fs::absolute(fs::path(Utf8ToWide(o.positional[0]))));
    else {
        fs::path base;
        if (o.project) base = fs::current_path();
        else {
            wchar_t* home = nullptr; size_t len = 0;
            if (_wdupenv_s(&home, &len, L"USERPROFILE") || !home) return Fail("USERPROFILE is not set; pass a target directory");
            base = home; free(home);
        }
        roots.push_back(base / L".claude" / L"skills");
        roots.push_back(base / L".agents" / L"skills");
    }
    json installed = json::array();
    for (const fs::path& root : roots) {
        fs::path dir = root / Utf8ToWide(kSkillName);
        std::error_code ec;
        fs::create_directories(dir, ec);
        fs::path file = dir / L"SKILL.md";
        std::ofstream out(file, std::ios::binary | std::ios::trunc);
        if (!out) return Fail("cannot write " + WideToUtf8(file.wstring()));
        out << text;
        installed.push_back(WideToUtf8(file.wstring()));
        if (!o.json) printf("installed %s\n", WideToUtf8(file.wstring()).c_str());
    }
    if (o.json) printf("%s\n", json({{"installed", installed}}).dump(2).c_str());
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
    if (o.command == "skill") return CmdSkill(o);

    static const char* kCommands[] = {"search", "packs", "status", "inspect", "compare", "export"};
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
    if (o.command == "compare") return CmdCompare(o, db);
    if (o.command == "export") return CmdExport(o, db);
    return CmdStatus(o, db);
}
