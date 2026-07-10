#pragma once
#include <string>
#include <vector>

// Performs a blocking OLE drag-and-drop of the given files (CF_HDROP).
// Must be called on the thread that called OleInitialize, outside an ImGui frame.
// Returns true if the drop completed (i.e. was not cancelled).
bool DoFileDrag(const std::vector<std::wstring>& paths);
