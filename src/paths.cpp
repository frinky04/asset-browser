#include "paths.h"
#include "util.h"
#include <shlobj.h>
#include <filesystem>
#include <fstream>

namespace fs = std::filesystem;

std::wstring AppDataDir(const wchar_t* sub) {
    PWSTR base = nullptr;
    std::wstring dir = L".";
    if (SUCCEEDED(SHGetKnownFolderPath(FOLDERID_LocalAppData, 0, nullptr, &base))) {
        dir = base;
        CoTaskMemFree(base);
    }
    return dir + L"\\AssetBrowser\\" + sub;
}

static fs::path RootSettingsPath() {
    return fs::path(AppDataDir(L"")) / L"asset-root.txt";
}

std::wstring LoadSavedAssetRoot() {
    std::ifstream in(RootSettingsPath(), std::ios::binary);
    std::string utf8((std::istreambuf_iterator<char>(in)), std::istreambuf_iterator<char>());
    while (!utf8.empty() && (utf8.back() == '\n' || utf8.back() == '\r')) utf8.pop_back();
    return Utf8ToWide(utf8);
}

void SaveAssetRoot(const std::wstring& root) {
    std::error_code ec;
    fs::create_directories(RootSettingsPath().parent_path(), ec);
    std::ofstream out(RootSettingsPath(), std::ios::binary | std::ios::trunc);
    out << WideToUtf8(root);
}
