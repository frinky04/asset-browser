#pragma once
#include <string>

// %LocalAppData%\AssetBrowser\<sub>
std::wstring AppDataDir(const wchar_t* sub);
std::wstring LoadSavedAssetRoot();
void SaveAssetRoot(const std::wstring& root);
