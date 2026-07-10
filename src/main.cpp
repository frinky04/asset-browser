#include "util.h"
#include "gfx.h"
#include "app.h"
#include "dragdrop.h"
#include "theme.h"

#include "imgui.h"
#include "imgui_internal.h" // ClearActiveID
#include "imgui_impl_win32.h"
#include "imgui_impl_dx11.h"

#include <ole2.h>
#include <shellapi.h>

extern IMGUI_IMPL_API LRESULT ImGui_ImplWin32_WndProcHandler(HWND, UINT, WPARAM, LPARAM);

static Gfx g_gfx;
static App g_app;
static bool g_inSizeMove = false;
static bool g_pendingResize = false;
static int g_resizeW = 0, g_resizeH = 0;

static LRESULT WINAPI WndProc(HWND hWnd, UINT msg, WPARAM wParam, LPARAM lParam) {
    if (ImGui_ImplWin32_WndProcHandler(hWnd, msg, wParam, lParam))
        return true;
    switch (msg) {
    case WM_SIZE:
        if (wParam != SIZE_MINIMIZED) {
            g_pendingResize = true;
            g_resizeW = LOWORD(lParam);
            g_resizeH = HIWORD(lParam);
        }
        return 0;
    case WM_DESTROY:
        PostQuitMessage(0);
        return 0;
    }
    return DefWindowProcW(hWnd, msg, wParam, lParam);
}

int WINAPI wWinMain(HINSTANCE hInstance, HINSTANCE, PWSTR, int) {
    // Default asset root; override with: asset-browser.exe --root <dir>
    std::wstring root = L"C:\\Users\\cuddl\\Documents\\Dev\\Games\\Assets";
    {
        int argc = 0;
        LPWSTR* argv = CommandLineToArgvW(GetCommandLineW(), &argc);
        for (int i = 1; i + 1 < argc; ++i)
            if (wcscmp(argv[i], L"--root") == 0)
                root = argv[i + 1];
        if (argv) LocalFree(argv);
    }

    if (FAILED(OleInitialize(nullptr)))
        return 1;

    ImGui_ImplWin32_EnableDpiAwareness();

    WNDCLASSEXW wc{ sizeof(wc), CS_CLASSDC, WndProc, 0, 0, hInstance,
                    LoadIconW(nullptr, IDI_APPLICATION), LoadCursorW(nullptr, IDC_ARROW),
                    nullptr, nullptr, L"AssetBrowserWnd", nullptr };
    RegisterClassExW(&wc);
    HWND hwnd = CreateWindowW(wc.lpszClassName, L"Asset Browser",
                              WS_OVERLAPPEDWINDOW, 100, 100, 1600, 950,
                              nullptr, nullptr, hInstance, nullptr);

    if (!g_gfx.Init(hwnd)) {
        MessageBoxW(hwnd, L"Failed to create D3D11 device.", L"Asset Browser", MB_ICONERROR);
        return 1;
    }

    ShowWindow(hwnd, SW_SHOWMAXIMIZED);
    UpdateWindow(hwnd);

    IMGUI_CHECKVERSION();
    ImGui::CreateContext();
    ImGuiIO& io = ImGui::GetIO();
    io.ConfigFlags |= ImGuiConfigFlags_NavEnableKeyboard;

    float dpiScale = ImGui_ImplWin32_GetDpiScaleForHwnd(hwnd);
    ImGuiStyle& style = ImGui::GetStyle();
    Theme::Apply(style);
    style.ScaleAllSizes(dpiScale);
    io.Fonts->AddFontFromFileTTF("C:\\Windows\\Fonts\\segoeui.ttf", floorf(17.0f * dpiScale));

    ImGui_ImplWin32_Init(hwnd);
    ImGui_ImplDX11_Init(g_gfx.device, g_gfx.ctx);

    g_app.Init(&g_gfx, hwnd, root);

    bool done = false;
    while (!done) {
        MSG msg;
        while (PeekMessageW(&msg, nullptr, 0, 0, PM_REMOVE)) {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
            if (msg.message == WM_QUIT) done = true;
        }
        if (done) break;

        if (g_pendingResize) {
            g_pendingResize = false;
            ImGui_ImplDX11_InvalidateDeviceObjects();
            g_gfx.Resize(g_resizeW, g_resizeH);
            ImGui_ImplDX11_CreateDeviceObjects();
        }

        ImGui_ImplDX11_NewFrame();
        ImGui_ImplWin32_NewFrame();
        ImGui::NewFrame();

        g_app.Frame();

        ImGui::Render();
        const float clear[4] = { 0.07f, 0.07f, 0.08f, 1.0f };
        g_gfx.ctx->OMSetRenderTargets(1, &g_gfx.backbufferRTV, nullptr);
        g_gfx.ctx->ClearRenderTargetView(g_gfx.backbufferRTV, clear);
        ImGui_ImplDX11_RenderDrawData(ImGui::GetDrawData());
        g_gfx.swap->Present(1, 0);

        // Deferred OLE drag: DoDragDrop blocks and pumps a modal message loop,
        // so it must run between ImGui frames.
        if (!g_app.pendingDrag.empty()) {
            std::vector<std::wstring> paths = std::move(g_app.pendingDrag);
            g_app.pendingDrag.clear();
            // OLE swallows WM_LBUTTONUP during the drag; tell ImGui the button
            // is up or the UI thinks LMB is held forever.
            io.AddMouseButtonEvent(ImGuiMouseButton_Left, false);
            DoFileDrag(paths);
            ImGui::ClearActiveID();
        }
    }

    g_app.Shutdown();
    ImGui_ImplDX11_Shutdown();
    ImGui_ImplWin32_Shutdown();
    ImGui::DestroyContext();
    g_gfx.Shutdown();
    DestroyWindow(hwnd);
    UnregisterClassW(wc.lpszClassName, hInstance);
    OleUninitialize();
    return 0;
}
