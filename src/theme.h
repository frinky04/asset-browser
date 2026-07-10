#pragma once
#include "imgui.h"

// Flat dark theme: near-black neutral surfaces, zero rounding, hairline borders,
// one warm amber accent used for selection/focus and nothing else.
namespace Theme {

inline ImVec4 Accent(float a = 1.0f) { return ImVec4(0.941f, 0.651f, 0.227f, a); } // #F0A63A

constexpr ImU32 kAccent          = IM_COL32(240, 166, 58, 255);
constexpr ImU32 kSelectionFill   = IM_COL32(240, 166, 58, 34);
constexpr ImU32 kSelectionBorder = IM_COL32(240, 166, 58, 190);
constexpr ImU32 kSplitterIdle    = IM_COL32(40, 40, 45, 255);
constexpr ImU32 kSplitterHover   = IM_COL32(240, 166, 58, 130);
constexpr ImU32 kSplitterActive  = IM_COL32(240, 166, 58, 255);
constexpr ImU32 kTileBg          = IM_COL32(26, 26, 29, 255);
constexpr ImU32 kTileBgHover     = IM_COL32(37, 37, 42, 255);
constexpr ImU32 kTextPrimary     = IM_COL32(233, 233, 236, 255);
constexpr ImU32 kTextDim         = IM_COL32(138, 138, 148, 255);

inline void Apply(ImGuiStyle& s) {
    // geometry: flat and square
    s.WindowRounding = 0.0f;
    s.ChildRounding = 0.0f;
    s.FrameRounding = 0.0f;
    s.PopupRounding = 0.0f;
    s.GrabRounding = 0.0f;
    s.TabRounding = 0.0f;
    s.ScrollbarRounding = 0.0f;
    s.WindowBorderSize = 0.0f;
    s.ChildBorderSize = 0.0f;
    s.PopupBorderSize = 1.0f;
    s.FrameBorderSize = 0.0f;
    s.WindowPadding = ImVec2(12, 12);
    s.FramePadding = ImVec2(10, 6);
    s.ItemSpacing = ImVec2(8, 6);
    s.ItemInnerSpacing = ImVec2(6, 4);
    s.ScrollbarSize = 12.0f;
    s.GrabMinSize = 8.0f;

    const ImVec4 bg       (0.071f, 0.071f, 0.078f, 1.0f);  // #121214 window base
    const ImVec4 bgAlt    (0.094f, 0.094f, 0.102f, 1.0f);  // #18181A popups / menubar
    const ImVec4 frame    (0.129f, 0.129f, 0.141f, 1.0f);  // #212124 inputs / buttons
    const ImVec4 frameHov (0.169f, 0.169f, 0.184f, 1.0f);  // #2B2B2F
    const ImVec4 frameAct (0.216f, 0.216f, 0.239f, 1.0f);  // #37373D
    const ImVec4 border   (0.149f, 0.149f, 0.169f, 1.0f);  // #26262B hairlines
    const ImVec4 text     (0.914f, 0.914f, 0.925f, 1.0f);  // #E9E9EC
    const ImVec4 textDim  (0.541f, 0.541f, 0.580f, 1.0f);  // #8A8A94

    ImVec4* c = s.Colors;
    c[ImGuiCol_Text] = text;
    c[ImGuiCol_TextDisabled] = textDim;
    c[ImGuiCol_WindowBg] = bg;
    c[ImGuiCol_ChildBg] = ImVec4(0, 0, 0, 0);
    c[ImGuiCol_PopupBg] = bgAlt;
    c[ImGuiCol_Border] = border;
    c[ImGuiCol_BorderShadow] = ImVec4(0, 0, 0, 0);
    c[ImGuiCol_FrameBg] = frame;
    c[ImGuiCol_FrameBgHovered] = frameHov;
    c[ImGuiCol_FrameBgActive] = frameAct;
    c[ImGuiCol_TitleBg] = bg;
    c[ImGuiCol_TitleBgActive] = bg;
    c[ImGuiCol_TitleBgCollapsed] = bg;
    c[ImGuiCol_MenuBarBg] = bgAlt;
    c[ImGuiCol_ScrollbarBg] = ImVec4(0, 0, 0, 0);
    c[ImGuiCol_ScrollbarGrab] = frameHov;
    c[ImGuiCol_ScrollbarGrabHovered] = frameAct;
    c[ImGuiCol_ScrollbarGrabActive] = Accent(0.85f);
    c[ImGuiCol_CheckMark] = Accent();
    c[ImGuiCol_SliderGrab] = Accent(0.85f);
    c[ImGuiCol_SliderGrabActive] = Accent();
    c[ImGuiCol_Button] = frame;
    c[ImGuiCol_ButtonHovered] = frameHov;
    c[ImGuiCol_ButtonActive] = frameAct;
    c[ImGuiCol_Header] = Accent(0.16f);          // selected Selectables (sidebar toggles)
    c[ImGuiCol_HeaderHovered] = Accent(0.24f);
    c[ImGuiCol_HeaderActive] = Accent(0.32f);
    c[ImGuiCol_Separator] = border;
    c[ImGuiCol_SeparatorHovered] = Accent(0.6f);
    c[ImGuiCol_SeparatorActive] = Accent();
    c[ImGuiCol_ResizeGrip] = ImVec4(0, 0, 0, 0);
    c[ImGuiCol_ResizeGripHovered] = Accent(0.5f);
    c[ImGuiCol_ResizeGripActive] = Accent();
    c[ImGuiCol_Tab] = frame;
    c[ImGuiCol_TabHovered] = frameHov;
    c[ImGuiCol_TextSelectedBg] = Accent(0.30f);
    c[ImGuiCol_DragDropTarget] = Accent(0.85f);
    c[ImGuiCol_NavCursor] = Accent();
    c[ImGuiCol_ModalWindowDimBg] = ImVec4(0, 0, 0, 0.5f);
}

} // namespace Theme
