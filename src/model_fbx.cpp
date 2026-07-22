#include "model.h"
#include "util.h"
#include <cfloat>
#include <cmath>
#include <filesystem>
#include <unordered_map>

#include "ufbx.h"
#include "stb_image.h"

namespace fs = std::filesystem;

// ---------------------------------------------------------------- FBX/OBJ CPU loading
//
// Same contract as LoadGlbCpu: pure CPU, worker-thread safe, vertices emitted
// pre-transformed to world space (right-handed Y-up, meters — ufbx converts).

namespace {

struct TexEntry { int image = -1; bool hasAlpha = false; };

int DecodeImage(CpuModel& out, const uint8_t* bytes, size_t size, bool& hadAlpha) {
    int w = 0, h = 0, comp = 0;
    stbi_uc* px = stbi_load_from_memory(bytes, (int)size, &w, &h, &comp, 4);
    if (!px) return -1;
    hadAlpha = (comp == 2 || comp == 4);
    CpuImage ci;
    ci.w = w; ci.h = h;
    ci.rgba.assign(px, px + (size_t)w * h * 4);
    stbi_image_free(px);
    out.images.push_back(std::move(ci));
    return (int)out.images.size() - 1;
}

std::wstring UfbxToWide(ufbx_string s) {
    return Utf8ToWide(std::string(s.data, s.length));
}

// Embedded content first, then external references: absolute path as written,
// relative path anchored at the model's directory, and finally just the
// basename next to the model (paths in packs often point at the author's disk).
TexEntry LoadUfbxTexture(CpuModel& out, std::unordered_map<const ufbx_texture*, TexEntry>& cache,
                         const ufbx_texture* tex, const fs::path& modelDir) {
    if (!tex) return {};
    auto it = cache.find(tex);
    if (it != cache.end()) return it->second;

    TexEntry e;
    if (tex->content.size > 0) {
        e.image = DecodeImage(out, (const uint8_t*)tex->content.data, tex->content.size, e.hasAlpha);
    } else {
        auto tryFile = [&](const std::wstring& p) {
            if (e.image >= 0 || p.empty()) return;
            std::vector<uint8_t> bytes;
            if (!ReadFileBytes(p, bytes) || bytes.empty()) return;
            e.image = DecodeImage(out, bytes.data(), bytes.size(), e.hasAlpha);
        };
        std::wstring abs = UfbxToWide(tex->absolute_filename);
        std::wstring rel = UfbxToWide(tex->relative_filename);
        std::wstring fn = UfbxToWide(tex->filename);
        tryFile(abs);
        if (!rel.empty()) tryFile((modelDir / fs::path(rel)).wstring());
        if (!fn.empty()) tryFile((modelDir / fs::path(fn).filename()).wstring());
        // Referenced paths often point at the author's disk while the real
        // texture sits in a shared dir above parallel format trees
        // (e.g. Models/other-formats/FBX/... vs Textures/). Walk up a few
        // levels trying the basename directly and under Textures/.
        fs::path base = fs::path(!rel.empty() ? rel : (!fn.empty() ? fn : abs)).filename();
        if (!base.empty()) {
            fs::path dir = modelDir;
            for (int up = 0; up < 4 && e.image < 0; ++up) {
                fs::path parent = dir.parent_path();
                if (parent.empty() || parent == dir) break;
                dir = parent;
                tryFile((dir / base).wstring());
                tryFile((dir / L"Textures" / base).wstring());
            }
        }
    }
    cache.emplace(tex, e);
    return e;
}

} // namespace

CpuModel LoadFbxObjCpu(const std::wstring& path) {
    CpuModel out;

    ufbx_load_opts opts{};
    opts.target_axes = ufbx_axes_right_handed_y_up;
    opts.target_unit_meters = 1.0f;
    opts.generate_missing_normals = true;
    opts.ignore_animation = true;
    opts.load_external_files = true;   // .mtl for OBJ counts as external
    opts.ignore_missing_external_files = true;
    opts.obj_search_mtl_by_filename = true;

    std::string utf8 = WideToUtf8(path);
    ufbx_error err{};
    ufbx_scene* scene = ufbx_load_file(utf8.c_str(), &opts, &err);
    if (!scene) return out;

    fs::path modelDir = fs::path(path).parent_path();
    std::unordered_map<const ufbx_texture*, TexEntry> texCache;
    out.bmin[0] = out.bmin[1] = out.bmin[2] = FLT_MAX;
    out.bmax[0] = out.bmax[1] = out.bmax[2] = -FLT_MAX;

    for (size_t mi = 0; mi < scene->meshes.count; ++mi) {
        const ufbx_mesh* mesh = scene->meshes.data[mi];
        if (mesh->num_triangles == 0) continue;

        std::vector<uint32_t> triIdx(mesh->max_face_triangles * 3);

        for (size_t ii = 0; ii < mesh->instances.count; ++ii) {
            const ufbx_node* node = mesh->instances.data[ii];
            if (!node->visible) continue;
            ufbx_matrix toWorld = node->geometry_to_world;
            ufbx_matrix nrmMat = ufbx_matrix_for_normals(&toWorld);

            for (size_t pi = 0; pi < mesh->material_parts.count; ++pi) {
                const ufbx_mesh_part& part = mesh->material_parts.data[pi];
                if (part.num_triangles == 0) continue;

                uint32_t indexOffset = (uint32_t)out.indices.size();

                for (size_t fi = 0; fi < part.num_faces; ++fi) {
                    ufbx_face face = mesh->faces.data[part.face_indices.data[fi]];
                    size_t numTris = ufbx_triangulate_face(triIdx.data(), triIdx.size(), mesh, face);
                    for (size_t t = 0; t < numTris * 3; ++t) {
                        uint32_t ix = triIdx[t];
                        CpuVertex cv{};
                        ufbx_vec3 p = ufbx_get_vertex_vec3(&mesh->vertex_position, ix);
                        p = ufbx_transform_position(&toWorld, p);
                        cv.px = (float)p.x; cv.py = (float)p.y; cv.pz = (float)p.z;
                        if (mesh->vertex_normal.exists) {
                            ufbx_vec3 n = ufbx_get_vertex_vec3(&mesh->vertex_normal, ix);
                            n = ufbx_transform_direction(&nrmMat, n);
                            float len = sqrtf((float)(n.x * n.x + n.y * n.y + n.z * n.z));
                            if (len > 1e-6f) {
                                cv.nx = (float)n.x / len; cv.ny = (float)n.y / len; cv.nz = (float)n.z / len;
                            } else {
                                cv.ny = 1.0f;
                            }
                        } else {
                            cv.ny = 1.0f;
                        }
                        if (mesh->vertex_uv.exists) {
                            ufbx_vec2 uv = ufbx_get_vertex_vec2(&mesh->vertex_uv, ix);
                            // ufbx UVs are bottom-left origin; renderer expects glTF top-left
                            cv.u = (float)uv.x; cv.v = 1.0f - (float)uv.y;
                        }
                        if (mesh->vertex_color.exists) {
                            ufbx_vec4 c = ufbx_get_vertex_vec4(&mesh->vertex_color, ix);
                            cv.r = (float)c.x; cv.g = (float)c.y; cv.b = (float)c.z; cv.a = (float)c.w;
                        } else {
                            cv.r = cv.g = cv.b = cv.a = 1.0f;
                        }
                        out.bmin[0] = (std::min)(out.bmin[0], cv.px); out.bmax[0] = (std::max)(out.bmax[0], cv.px);
                        out.bmin[1] = (std::min)(out.bmin[1], cv.py); out.bmax[1] = (std::max)(out.bmax[1], cv.py);
                        out.bmin[2] = (std::min)(out.bmin[2], cv.pz); out.bmax[2] = (std::max)(out.bmax[2], cv.pz);
                        out.indices.push_back((uint32_t)out.verts.size());
                        out.verts.push_back(cv);
                    }
                }

                CpuPrim cp;
                cp.indexOffset = indexOffset;
                cp.indexCount = (uint32_t)(out.indices.size() - indexOffset);
                if (cp.indexCount == 0) continue;
                const ufbx_material* mat =
                    (part.index < mesh->materials.count) ? mesh->materials.data[part.index] : nullptr;
                if (mat) {
                    // OBJ (and some FBX shaders) only fill the fbx-style maps, not pbr
                    const ufbx_material_map& colorMap =
                        (mat->pbr.base_color.has_value || mat->pbr.base_color.texture)
                            ? mat->pbr.base_color : mat->fbx.diffuse_color;
                    if (colorMap.has_value) {
                        ufbx_vec4 c = colorMap.value_vec4;
                        cp.baseColor[0] = (std::min)(1.0f, (std::max)(0.0f, (float)c.x));
                        cp.baseColor[1] = (std::min)(1.0f, (std::max)(0.0f, (float)c.y));
                        cp.baseColor[2] = (std::min)(1.0f, (std::max)(0.0f, (float)c.z));
                        cp.baseColor[3] = (std::min)(1.0f, (std::max)(0.0f, (float)c.w));
                    }
                    TexEntry te = LoadUfbxTexture(out, texCache, colorMap.texture, modelDir);
                    cp.image = te.image;
                    // FBX has no explicit alpha mode; cut out wherever the texture carries alpha
                    cp.alphaMask = (te.image >= 0 && te.hasAlpha);
                }
                out.prims.push_back(cp);
            }
        }
    }

    ufbx_free_scene(scene);
    if (out.verts.empty() || out.indices.empty()) return out;
    out.valid = true;
    return out;
}
