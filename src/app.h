#pragma once
#include "gfx.h"
#include "assets.h"
#include "thumbs.h"
#include "audio.h"
#include "model.h"
#include "search.h"
#include "embed.h"
#include "paths.h"
#include <mutex>
#include <condition_variable>
#include <deque>
#include <set>
#include <string>
#include <vector>
#include <unordered_map>

struct App {
    // systems
    Gfx* gfx = nullptr;
    HWND hwnd = nullptr;
    AssetDatabase db;
    Thumbs thumbs;
    AudioSystem audio;
    ModelRenderer renderer;

    // filter state (toggles; nothing selected = show everything)
    char searchBuf[256] = "";
    bool typeSel[3] = { false, false, false };  // indexed by AssetType
    std::set<std::string> selectedPacks;
    int lastTypeClicked = -1;
    std::string lastPackClicked;
    bool filterDirty = true;
    std::vector<int> filtered;      // indices into db.entries, display order
    std::string lastQuery;
    std::vector<int> lastCandidates;
    bool lastTypeSel[3] = { false, false, false };
    std::set<std::string> lastSelectedPacks;
    size_t totalFilterMatches = 0;
    ParsedQuery parsedQuery;
    int resultTypeCounts[3] = { 0, 0, 0 };
    std::unordered_map<std::string, int> resultExtCounts;

    // selection / preview
    std::set<int> selection;
    int lastClicked = -1;
    int previewIndex = -1;
    ID3D11ShaderResourceView* previewTexSRV = nullptr; // texture preview (at export size when downres active)
    int previewTexW = 0, previewTexH = 0;              // dims of the previewed image
    int previewTexSrcW = 0, previewTexSrcH = 0;        // source file dims
    int previewTexBuiltOpt = 0;                        // texDragOpt the preview was built with
    PcmData previewPcm;                                // decoded audio for waveform/selection
    ID3D11ShaderResourceView* previewWaveSRV = nullptr;
    float audioSelA = -1.0f, audioSelB = -1.0f;        // selection as fractions, <0 = none
    bool audioTuneLoop = false;
    float audioLoopCrossfadeMs = 50.0f;
    GpuModel previewModel;
    bool previewModelValid = false;
    OrbitCamera previewCam;
    OffscreenTarget previewRT;
    float texZoom = 0.0f;           // 0 = fit
    float texPanX = 0, texPanY = 0;
    bool texTile = false;           // tiling (wrap) preview mode

    // embedding search (ui_embed.cpp)
    enum class SemanticKind : uint8_t { None, Text, Image, Audio, Asset };
    EmbedSettings embedSettings;
    EmbedIndex embedIndex;
    EmbedIndexer embedIndexer;
    std::string embedKey;
    bool embedKeyFromEnv = false;
    bool embedWindowOpen = false;
    bool embedIndexLoaded = false;
    bool embedConfirmRebuild = false;       // modal pending
    int  embedRebuildTypes = 0;
    int  embedMissingCount = 0, embedStaleCount = 0; // refreshed with the other counts
    bool semanticSearch = false;
    SemanticKind semanticKind = SemanticKind::None;
    std::string semanticLabel;                 // what the current ranking is relative to
    std::vector<float> semanticQuery;          // normalised, embedIndex.dims
    std::vector<float> semanticScore;          // per db.entries index; valid while semantic ranking active
    std::unordered_map<std::string, std::vector<float>> semanticTextCache;
    std::string semanticError;
    std::string semanticNotice;               // transient hint (e.g. "clipboard has no image")
    double semanticNoticeUntil = 0;
    bool semanticBusy = false;
    ID3D11ShaderResourceView* semanticImageSRV = nullptr; // preview of a pasted/dropped query image
    int semanticImageW = 0, semanticImageH = 0;
    int semanticAssetIndex = -1;              // source texture of a "find similar" query
    // refreshed ~1/s in PumpEmbed
    int embedIndexableCount = 0, embedUnindexedCount = 0;
    int embedUnindexedTex = 0, embedUnindexedAudio = 0, embedUnindexedModel = 0;
    int embedIndexedByType[3] = { 0, 0, 0 };  // current vectors per AssetType
    // Folders (relative, lowercase, forward slashes, no trailing slash) kept out
    // of indexing and rankings. Per library, stored beside the index.
    std::vector<std::string> embedExcludes;
    char embedExcludeBuf[512] = "";
    bool EmbedExcluded(const AssetEntry& e) const;
    bool EmbedFolderExcluded(const std::string& relFolder) const; // exact entry present
    void EmbedSetFolderExcluded(const std::string& relFolder, bool excluded);
    void EmbedSaveExcludes() const;
    static std::string EmbedParentFolder(const AssetEntry& e); // "" for root-level files
    double embedAudioSecondsKnown = 0;        // capped seconds of un-indexed audio with probed durations
    int embedAudioUnknown = 0;                // un-indexed audio still awaiting a duration probe
    // audio duration probing (for the cost estimate)
    std::unordered_map<std::string, double> audioDurations; // relPath -> seconds (0 = unreadable)
    std::thread embedProbeThread;
    std::mutex embedProbeMutex;
    std::vector<std::pair<std::string, double>> embedProbeResults;
    bool embedProbing = false, embedProbeDone = false;
    double embedCountedAt = -1;
    size_t embedCountedEntries = 0;
    bool embedIndexerWasRunning = false;
    // async one-shot jobs (text/image query embed, price fetch, key test)
    struct EmbedAsync {
        std::thread thread;
        std::mutex m;
        bool ready = false;
        uint64_t id = 0;
        EmbedResult result;
        std::string label;
        SemanticKind kind = SemanticKind::None;
    } semanticJob;
    uint64_t semanticJobId = 0;
    std::thread embedPriceThread;
    std::mutex embedPriceMutex;
    bool embedPriceReady = false, embedPriceFetched = false, embedPriceFetching = false;
    EmbedClient::Prices embedPrices;
    std::string embedPriceError;
    std::string embedPriceModel;
    std::thread embedKeyTestThread;
    std::mutex embedKeyTestMutex;
    bool embedKeyTestReady = false, embedKeyTesting = false;
    std::string embedKeyTestResult;
    char embedKeyBuf[256] = "";
    char embedModelBuf[128] = "";

    void EmbedInit();                       // load settings + key
    void EmbedLoadIndexForRoot();           // after root change
    void EmbedShutdown();
    void PumpEmbed();                       // main thread, per frame
    void DrawEmbedWindow();
    enum EmbedTypes { EmbedTex = 1, EmbedAudio = 2, EmbedModel = 4, EmbedAll = 7 };
    void EmbedStartIndexing(bool rebuild, int types = EmbedAll);
    // Subset indexing: only un-indexed, indexable, non-excluded entries among `candidates`.
    struct EmbedEstimate { int count = 0; double cost = 0; bool upperBound = false; };
    EmbedEstimate EmbedEstimateFor(const std::vector<int>& candidates) const;
    std::vector<int> EmbedPendingIn(const std::vector<int>& candidates) const;
    std::vector<int> EmbedEntriesUnderFolder(const std::string& relFolder) const; // "" = pack-level match on e.pack
    bool EmbedStartIndexingSubset(std::vector<int> candidates, const std::string& what);
    void EmbedIndexMenuItem(const std::vector<int>& candidates, const char* what); // one MenuItem with count + cost
    std::unordered_map<std::string, std::pair<int, int>> embedPackCoverage; // pack -> {indexed, indexable}
    std::vector<int> EmbedUnindexed(int types = EmbedAll) const; // indexable entries lacking a current vector
    void EmbedKeepStaleVectors();           // adopt changed files' new mtime/size, keeping their vectors
    void EmbedProbeDurations();             // kick off / collect background duration probes
    // Model contact sheets need the GPU: workers queue a loaded CpuModel and block
    // until the main thread has rendered it (PumpEmbed, a few per frame).
    struct SheetRequest {
        CpuModel model;
        std::vector<uint8_t> rgba;  // sheet, kEmbedModelView*cols x kEmbedModelView*rows
        bool done = false, ok = false;
    };
    std::mutex sheetMutex;
    std::condition_variable sheetCv;
    std::deque<SheetRequest*> sheetQueue;
    bool sheetAbort = false;
    OffscreenTarget embedSheetRT;
    bool RenderSheetRequest(const EmbedJob& job, std::vector<uint8_t>& rgba); // worker side
    void PumpSheetRenders();                                                 // main thread
    void EmbedStopIndexing();               // cancel + unblock sheet waits + join (main thread)
    std::wstring EmbedIndexPathForRoot() const;
    bool SemanticReady() const { return !embedKey.empty() && embedIndexLoaded && !embedIndex.Empty(); }
    void SemanticQueryText(const std::string& text);
    void SemanticQueryImage(const uint8_t* rgba, int w, int h, const std::string& label);
    void SemanticQueryAudio(const PcmData& pcm, uint64_t f0, uint64_t f1, const std::string& label);
    void SemanticQueryPreviewSelection();   // audio preview: selected range (or whole clip)
    void SemanticQueryAsset(int index);     // "find similar": uses stored vector, no API call
    void SemanticClear();
    void SemanticNotify(const std::string& text);
    bool SemanticFindSimilar(int index);    // S / menu: similar to an indexed asset, with user feedback
    bool SemanticActive() const { return semanticSearch && semanticKind != SemanticKind::None && !semanticQuery.empty(); }
    bool PasteImageQuery();                 // clipboard bitmap or file -> image query
    bool DropFileQuery(const std::wstring& file);  // dropped/copied image or audio file -> query
    std::wstring pendingDropFile;           // set by WM_DROPFILES (main.cpp), consumed in Frame

    // drag-out (executed by main.cpp after the ImGui frame)
    std::vector<std::wstring> pendingDrag;
    bool requestFolderPicker = false;

    // settings
    float tileSize = 200.0f;
    float previewRatio = 0.35f;      // preview share of the grid/preview area
    bool focusSearch = false;
    bool searchHasFocus = false;
    bool selectSearchAll = false;
    bool focusGrid = false;
    int scrollToIndex = -1;
    int sortMode = 0;               // 0 relevance, 1 name, 2 newest, 3 largest
    std::string dragFormatExt;      // preferred model format for drag-out ("" = canonical/GLB)
    int texDragOpt = 0;             // texture drag size: 0=original, -2=half, -4=quarter, >0=max side px

    // drag-time exports (resized textures, audio clips) land here
    std::wstring exportDir;

    void Init(Gfx* g, HWND wnd, const std::wstring& root);
    void Shutdown();
    void Frame();                   // build the whole UI (between NewFrame/Render)
    void SetAssetRoot(const std::wstring& root);

    // internals
    void DrawMenuBar(float& menuH);
    void DrawSidebar();
    void DrawGrid();                // ui_grid.cpp
    void DrawPreview();             // ui_preview.cpp
    void DrawStatusBar(float h);

    void SetPreview(int index);     // ui_preview.cpp
    void TogglePreviewAudio();
    void HandleTileClick(int index, bool ctrl, bool shift);
    void BeginDragOfSelection(int draggedIndex);
    std::vector<std::wstring> DragPathsFor(int index);
    std::wstring ExportResizedTexture(const AssetEntry& e); // returns path to drag ("" = use original)
    std::wstring ExportAudioSelection();                    // selection (or whole clip) as wav
    bool AudioSelectionFrames(uint64_t& f0, uint64_t& f1) const;
    uint64_t AudioLoopCrossfadeFrames(uint64_t f0, uint64_t f1) const;
    PcmData BuildTunedAudioLoop() const;
    bool DragDims(int w, int h, int& nw, int& nh) const;    // texDragOpt applied; true if downscaled
    void BuildTexturePreview();                             // ui_preview.cpp
    void OpenInExplorer(int index);
    void CopyPathToClipboard(int index);
    void RebuildFilterIfDirty();    // search.cpp
};

bool PickAssetRoot(HWND owner, std::wstring& root);
