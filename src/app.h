#pragma once
#include "gfx.h"
#include "assets.h"
#include "thumbs.h"
#include "audio.h"
#include "model.h"
#include <set>
#include <string>
#include <vector>

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

    // drag-out (executed by main.cpp after the ImGui frame)
    std::vector<std::wstring> pendingDrag;

    // settings
    float tileSize = 200.0f;
    float previewRatio = 0.35f;      // preview share of the grid/preview area
    bool focusSearch = false;
    std::string dragFormatExt;      // preferred model format for drag-out ("" = canonical/GLB)
    int texDragOpt = 0;             // texture drag size: 0=original, -2=half, -4=quarter, >0=max side px

    // drag-time exports (resized textures, audio clips) land here
    std::wstring exportDir;

    void Init(Gfx* g, HWND wnd, const std::wstring& root);
    void Shutdown();
    void Frame();                   // build the whole UI (between NewFrame/Render)

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
