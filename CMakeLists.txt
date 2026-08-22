cmake_minimum_required(VERSION 3.24)
project(asset_browser VERSION 0.1.0 LANGUAGES C CXX)

set(ASSET_BROWSER_VERSION "${PROJECT_VERSION}" CACHE STRING "Version embedded in release builds")
set(CMAKE_MSVC_RUNTIME_LIBRARY "MultiThreaded$<$<CONFIG:Debug>:Debug>")

set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

include(FetchContent)

FetchContent_Declare(imgui
  GIT_REPOSITORY https://github.com/ocornut/imgui.git
  GIT_TAG        v1.92.1-docking
  GIT_SHALLOW    TRUE)
FetchContent_Declare(stb
  GIT_REPOSITORY https://github.com/nothings/stb.git
  GIT_TAG        master
  GIT_SHALLOW    TRUE)
FetchContent_Declare(cgltf
  GIT_REPOSITORY https://github.com/jkuhlmann/cgltf.git
  GIT_TAG        v1.14
  GIT_SHALLOW    TRUE)
FetchContent_Declare(miniaudio
  GIT_REPOSITORY https://github.com/mackron/miniaudio.git
  GIT_TAG        0.11.22
  GIT_SHALLOW    TRUE
  SOURCE_SUBDIR  _does_not_exist_) # populate only; we compile miniaudio ourselves
FetchContent_Declare(nlohmann_json
  GIT_REPOSITORY https://github.com/nlohmann/json.git
  GIT_TAG        v3.11.3
  GIT_SHALLOW    TRUE)
FetchContent_Declare(ufbx
  GIT_REPOSITORY https://github.com/ufbx/ufbx.git
  GIT_TAG        v0.23.0
  GIT_SHALLOW    TRUE)

FetchContent_MakeAvailable(imgui stb cgltf miniaudio ufbx nlohmann_json)

add_library(imgui_lib STATIC
  ${imgui_SOURCE_DIR}/imgui.cpp
  ${imgui_SOURCE_DIR}/imgui_draw.cpp
  ${imgui_SOURCE_DIR}/imgui_tables.cpp
  ${imgui_SOURCE_DIR}/imgui_widgets.cpp
  ${imgui_SOURCE_DIR}/backends/imgui_impl_win32.cpp
  ${imgui_SOURCE_DIR}/backends/imgui_impl_dx11.cpp)
target_include_directories(imgui_lib PUBLIC ${imgui_SOURCE_DIR} ${imgui_SOURCE_DIR}/backends)
target_compile_definitions(imgui_lib PUBLIC UNICODE _UNICODE NOMINMAX WIN32_LEAN_AND_MEAN)

# Windowless core: scanning, search, embeddings, audio/model decoding, D3D helpers. Shared by
# the GUI and the headless CLI.
add_library(abcore STATIC
  src/paths.cpp
  src/assets.cpp
  src/search.cpp
  src/gfx.cpp
  src/model.cpp
  src/model_fbx.cpp
  src/audio.cpp
  src/embed.cpp
  src/preview.cpp
  src/third_party_impl.cpp
  src/stb_vorbis_impl.c
  ${ufbx_SOURCE_DIR}/ufbx.c)
target_include_directories(abcore PUBLIC
  src
  ${stb_SOURCE_DIR}
  ${cgltf_SOURCE_DIR}
  ${miniaudio_SOURCE_DIR}
  ${ufbx_SOURCE_DIR})
target_link_libraries(abcore PUBLIC nlohmann_json::nlohmann_json d3d11 dxgi d3dcompiler ole32 shell32 shlwapi winhttp crypt32)
target_compile_options(abcore PRIVATE /utf-8 /EHsc /MP)
target_compile_definitions(abcore PUBLIC UNICODE _UNICODE NOMINMAX WIN32_LEAN_AND_MEAN)

add_executable(asset-browser WIN32
  src/main.cpp
  src/app.cpp
  src/thumbs.cpp
  src/ui_grid.cpp
  src/ui_preview.cpp
  src/dragdrop.cpp
  src/ui_embed.cpp)

string(REPLACE "." "," ASSET_BROWSER_VERSION_COMMA "${ASSET_BROWSER_VERSION}")
configure_file(src/version.rc.in version.rc @ONLY)
target_sources(asset-browser PRIVATE "${CMAKE_CURRENT_BINARY_DIR}/version.rc")

target_link_libraries(asset-browser PRIVATE abcore imgui_lib)
target_compile_options(asset-browser PRIVATE /utf-8 /EHsc /MP)

# Headless CLI for scripts and coding agents (console subsystem).
add_executable(ab src/cli.cpp)
target_link_libraries(ab PRIVATE abcore)
target_compile_options(ab PRIVATE /utf-8 /EHsc /MP)
target_compile_definitions(ab PRIVATE ASSET_BROWSER_VERSION="${ASSET_BROWSER_VERSION}")
