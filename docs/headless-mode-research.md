# Headless mode research

Date: 2026-08-23. Goal: a headless / CLI mode so coding agents (Claude Code, Codex CLI, Cursor, Copilot, Gemini CLI) can find and pull assets from a user's library while building a game.

This doc has three parts: what other apps do, what agents need, and what that means for this codebase.

---

## 1. How other apps do headless / CLI

### GUI apps with a headless path

| App | Invocation | GUI vs headless | Output / exit codes |
|---|---|---|---|
| Blender | `blender -b file.blend -P script.py` | One binary; `-b` suppresses the window, Python API unchanged | Files via `-o`; script failure only fails the process if `--python-exit-code N` is set. Args execute in order (`-f 1 -o out` renders before `-o` applies) |
| Godot | `godot --headless --script x.gd --quit` | Headless = driver substitution (null display + dummy audio), same engine loop | `SceneTree.quit(code)`; `--quiet` silences stdout, errors still print |
| Chrome | `chrome --headless --dump-dom / --screenshot / --print-to-pdf` | Unified headless + headful in one binary (since 132; legacy split out to `chrome-headless-shell`) | `--dump-dom` → stdout, others → files, `--remote-debugging-port` turns it into a server |
| Unity | `Unity -batchmode -nographics -quit -executeMethod X` | Flags on the editor | Log file, not stdout (CI pain point); exception → exit 1 |
| GIMP / Krita | `gimp -i -b "(script)"` / `krita in.png --export --export-filename out.jpg` | `-i` no interface; `-c` routes errors from dialogs to console | File-only for Krita |
| VS Code | `code .`, `--wait`, `--install-extension` | `code.cmd` is a 7-line shim: runs `Code.exe` in Node mode (`ELECTRON_RUN_AS_NODE=1`) for cheap commands, spawns the detached GUI otherwise. `--wait` = temp marker file the GUI deletes | Exit code propagated |
| Obsidian CLI | `obsidian search query=x format=json` | CLI is a *client* of the running GUI (launches it if needed); Windows uses an `Obsidian.com` console redirector | `format=json|tsv|csv` |

Sources: Blender manual (command line arguments), Godot CLI tutorial, developer.chrome.com/docs/chromium/headless, Unity EditorCommandLineArguments, docs.gimp.org, docs.krita.org, github.com/microsoft/vscode `resources/win32/bin/code.cmd` + `src/vs/code/node/cli.ts`, obsidian.md/help/cli.

Two lessons: (1) nobody forks the codebase — it's one binary with a flag, or a thin launcher; (2) the ones people praise (Chrome `--dump-dom`, Obsidian `format=json`) put machine-readable data on stdout; the ones people curse (Unity) put it in a log file.

### CLI design rules (clig.dev, Heroku 12-factor CLI, `gh`, ripgrep, NDJSON)

- stdout = data (human or `--json`), stderr = progress/logs/errors. Never mix.
- Exit 0 success, non-zero failure; `gh` uses 0 / 1 / 2 cancelled / 4 needs-auth. Errors should say what to do next.
- `--json` flag; auto-detect non-TTY to drop colour/spinners (`NO_COLOR`, `TERM=dumb`).
- `gh --json a,b,c` takes an explicit field list (stable, discoverable schema), plus built-in `--jq` and `--template`.
- ripgrep `--json` is NDJSON with typed envelopes (`begin`/`match`/`end`/`summary`) and `{text}` vs `{bytes: base64}` for non-UTF-8 paths. Copy the typed-envelope + terminal-summary idea for streaming scan/index progress.
- Subcommands over flags for verbs; `--help` with examples; first output within ~100 ms; `--quiet`, `--dry-run`; never prompt unless stdin is a TTY and always allow a flag override.

### Windows: GUI-subsystem exe vs the console

Our exe is `/SUBSYSTEM:WINDOWS` (`add_executable(asset-browser WIN32 …)`), so it has no console and `cmd`/PowerShell return immediately instead of waiting. Options:

| Option | Verdict |
|---|---|
| `AttachConsole(ATTACH_PARENT_PROCESS)` + `freopen("CONOUT$")` | Hack. Shell doesn't wait so prompt interleaves with output; redirection (`> file`) and pipes break unless you special-case inherited handles. OK only as a fallback for `--help`/`--version`. |
| `/SUBSYSTEM:CONSOLE` + `FreeConsole()` when launched from Explorer | Console window flashes on double-click. |
| **`.com` shim** (`asset-browser.com`: tiny console exe that spawns `asset-browser.exe` with inherited stdio, waits, forwards exit code) | What Visual Studio (`devenv.com`) and Obsidian do. Typing `asset-browser` resolves `.com` first by PATHEXT. Clean: blocking, pipes, redirection, exit codes all work. Two binaries. |
| **Separate console exe** (`asset-browser-cli.exe` / `ab.exe`) linking the same core | Cleanest; requires factoring the core into a static lib. Ollama ships exactly this. |

Sources: Raymond Chen "How do I write a program that can be run either as a console or a GUI application?", MS AttachConsole docs, devenv.com docs, speedyleion.github.io WinMain-and-stdout.

### One-shot vs `serve`

Ollama (`ollama serve`, keeps models warm 5 min), LSP servers (JSON-RPC over stdio, `Content-Length` framing), and MCP stdio servers (stdout must carry only protocol messages, logging to stderr) exist because of expensive warm state or streaming/bidirectional needs. Rule: one-shot commands when startup is cheap and calls are independent; add `serve` when there is warm state, streaming progress, or multiple clients.

Relevant here: our scan has **no on-disk cache** (`assets.cpp` always re-walks), and the `.abe` vector index is loaded per run. For a 100k-file library a one-shot `search` could take seconds just scanning. Either add a scan cache or add a `serve` mode (see §3).

---

## 2. What agents need

### CLI vs MCP — the evidence

- MCP tool schemas are injected every turn. Measured: GitHub ops via CLI ≈ 224 tokens/op with zero fixed cost; via MCP ≈ 3k tokens of schema *per prompt* — a 20-prompt session with 2 real ops cost 448 vs 61,654 tokens (Mornati). Scalekit/Firecrawl: 1.4–8.7k (CLI) vs 32–83k (MCP) for the same tasks, with higher CLI success rate. Rule of thumb: native MCP only if the tool is used in >40% of prompts; for sporadic use, CLI + on-demand skill doc.
- Anthropic's own "Code execution with MCP" post concedes both problems (schema bloat, results round-tripping through the model) and pushes toward progressively-discovered, code-callable tools; Agent Skills are the productised version (name+description loaded first, body only when relevant).
- MCP still wins on: `image` content blocks (Claude Code shows them to the model directly), typed `outputSchema`/`isError`, and being the only option for Claude Desktop users with no terminal.
- Block's Goose reviewers note MCP tools don't pipe into each other the way shell commands do.

Sources: anthropic.com/engineering/writing-tools-for-agents, anthropic.com/engineering/code-execution-with-mcp, blog.mornati.net (MCP vs CLI data), firecrawl.dev/blog/mcp-vs-cli, blog.arcjet.com/designing-a-cli-for-ai-agents, garbas.si "AI agents are your new users: CLI", platform.claude.com Agent Skills best practices, modelcontextprotocol.io tools spec.

**Conclusion:** CLI first, thin MCP wrapper second (same core, ~4 tools), plus a `SKILL.md` / AGENTS.md snippet and a `--help` that teaches the same thing.

### Output rules for agents (sourced, concrete)

1. **JSON when stdout isn't a TTY** (Arcjet default) or when `CLAUDECODE=1`; keep `--json` explicit too.
2. **Small by default with a knob.** Anthropic: pagination, filtering, truncation with sensible defaults; a `concise|detailed` switch (their example: 72 vs 206 tokens). Default ~10 results, `--limit/--offset`.
3. **Stable IDs + readable names.** Anthropic found resolving opaque IDs to names "significantly improves precision". Return both. ID must survive re-scans → hash of relPath (or content), not row index.
4. **Absolute paths, always** (agent cwd is unpredictable).
5. **Errors that say what to do next**: `No results for "mossy stone". Try --semantic, broader terms, or 'ab packs' to see what's indexed.`
6. **Non-interactive**; `--yes` for overwrites.
7. **Contract stability**: add fields/flags, never rename.
8. **Images the model can see.** CLI: write a PNG and print its path — Claude Code's `Read` renders PNGs. A **contact sheet of the top-N results with IDs burned in** is the highest-value feature here: one image, one Read, one decision. MCP: return an `image` content block (256 px JPEG to limit image tokens).
9. stdout pristine; scan/index progress to stderr (as NDJSON if `--json`).

### Tool description guidance (Anthropic)

Describe tools like onboarding a new hire; unambiguous param names (`asset_id`, `max_results`); namespace (`assets_search`, `assets_inspect`, `assets_preview`, `assets_export`); few consolidated tools (one `search` with `--type`, not three list commands); say when *not* to use it and what it costs; run 10–20 realistic transcript evals and iterate descriptions.

### Prior art (asset MCPs/CLIs)

| Project | Shape | Takeaway |
|---|---|---|
| Sketchfab MCP (gregkop) | `search` → `model-details` → `download(modelId, format, outputPath)` | The canonical triad |
| Freesound MCP (several) | one `search_sounds` returning name, duration, format, tags, **license**, preview URLs | License + duration inline so the agent decides in one call; forks added download |
| blender-mcp (ahujasid) | Poly Haven / Sketchfab search+download, a separate *preview-image* tool, `get_*_status` tools | Preview-as-image tool; status tool to check if a feature is enabled |
| polyhaven-skill (MaksPyn) | Agent Skill + Node CLI | Closest to the recommended shape |
| Godot-MCP (42 tools), Unity MCP forks (30–268 tools) | Editor control | Tool-count inflation is the recurring complaint |

Complaints in the wild: API keys required for everything (a local library avoids this), huge tool counts, agents ignoring engine import settings, and no visual verification loop. Writeups on agents making games (Chier Hu, Vaughan, godogen) agree: text-serialised engines work well, "the visual/asset frontier remains unsolved" without vision tooling, and import conventions belong in AGENTS.md.

### The workflow to support

1. **Search by description** — fuzzy + semantic under one verb; return id, name, type, abs path, key dims, score; optional contact sheet.
2. **Inspect** — texture: WxH, channels, tiles?, sibling PBR maps; model: format, tri/vert count, bounds, materials, rig/anims; audio: duration, sample rate, channels, trimmed duration. `--preview out.png`.
3. **Export into the project** — `--to <dir>`, `--resize 1024`, `--format glb|png|wav`, `--trim-silence`, loop export; optional `--engine godot` hint for post-steps. Return output paths as JSON.
4. **Record what was used** — append to `assets-used.json` / `CREDITS.md` automatically (agents forget).
5. **Fit the engine** — filters like `--max-size 1024`, `--max-tris 5000`, `--max-duration 2s` so a 4K texture doesn't land in a Phaser sprite.

---

## 3. What this means for this codebase

### What's already GUI-free (link into a CLI as-is)

- `AssetDatabase::StartScan/PumpScan` — `assets.cpp` (thread + `recursive_directory_iterator`; no D3D).
- All of `search.cpp`: `ParseQueryText`, `FuzzyMatch`, `MatchEntry`, `SemanticTextFromQuery` — zero ImGui.
- All of `embed.cpp`: `EmbedIndex` load/cosine, `EmbedClient::EmbedText`, `LoadApiKey` (DPAPI + `OPENROUTER_API_KEY`), settings/index paths under `%LOCALAPPDATA%\AssetBrowser\embeddings`.
- `audio.cpp`: `DecodeAudioFile`, `AudioDurationSeconds`, `WriteWav16`, `BuildSeamlessLoop`, waveform render.
- `model.cpp`/`model_fbx.cpp`: `LoadGlbCpu`, `LoadFbxObjCpu` (tri count = `indices.size()/3`, bounds already present).
- `thumbs.cpp` cache layer + `LoadOrMakeTextureThumb` (CPU, stb).
- `App::ExportResizedTexture` (`app.cpp:493`) and `App::DragPathsFor` (`app.cpp:588`, model variant selection) are pure apart from reading `App` members.

### Needs refactor

- `App::RebuildFilterIfDirty` (`search.cpp:243`) — extract to a free `Filter(entries, query, typeSel, packs, index, excludes, sort) -> vector<int>`; ~100 lines, no ImGui inside.
- `SemanticQueryText` / `PumpEmbed` / `SemanticNotify` (`ui_embed.cpp`) — only ImGui dependency is `ImGui::GetTime()`; swap for `steady_clock`.
- `ExportAudioSelection` — takes preview state; parameterise on `PcmData` + range.
- Embed exclusions / index-path helpers are `App` methods purely for state.
- **Genuinely needs D3D**: model thumbnails, contact sheets (`RenderModelSheet`), model embedding. `Gfx::Init` needs an HWND for the swap chain; a headless device would need `D3D11CreateDevice` without a swap chain (plus an offscreen target — `OffscreenTarget` already exists). `--dump-sheet` in `main.cpp:84` is the precedent: it creates a hidden window + device, renders, exits.

### No metadata cache exists

`AssetEntry` holds only path/name/ext/pack/type/mtime/size/variants. Dimensions, channels, tri count, duration are computed transiently on preview. An `inspect` command must decode per asset (all decoders are GUI-free, cheap), and a `search --max-size` filter needs either per-call decode of candidates or a persisted metadata cache keyed by (path, mtime, size) — the same key the thumb and embed caches already use.

### Recommended design

**Binary layout.** Factor `src/{assets,search,embed,audio,model,model_fbx,thumbs-cache}` into a static lib `abcore`; keep `asset-browser.exe` (WIN32) and add a console target `ab.exe` (or `asset-browser.com` shim that forwards to `asset-browser.exe --cli …` if you'd rather keep one exe). The separate console exe is the cleaner choice; the shim is the smaller diff. Either way D3D-needing commands (model previews/sheets) can spawn the GUI exe with a hidden window the way `--dump-sheet` already does.

**Verbs** (few, consolidated):

```
ab search <query> [--type tex|model|audio] [--pack P] [--semantic|--fuzzy|--hybrid]
                  [--similar <id>] [--limit 10] [--offset 0] [--max-size 1024]
                  [--max-tris N] [--max-duration 2s] [--sheet out.png] [--json]
ab inspect <id|path> [--preview out.png] [--json]
ab export  <id|path> --to <dir> [--resize 1024] [--format glb|png|wav|ogg]
                     [--trim-silence] [--loop a,b] [--name X] [--yes] [--json]
ab packs / ab status        # what's indexed, semantic available?, root, counts
ab index [--budget 2.00]    # run semantic indexing headless (already GUI-free)
ab serve                    # optional: stdio MCP with the same 4 tools
ab skill                    # print SKILL.md / AGENTS.md snippet
```

Query syntax stays the existing `type:` `pack:` `ext:` `size:` `age:` `path:` mini-language so the docs are shared with the GUI.

**Output.** JSON on non-TTY / `--json`; one object per result with `id` (fnv1a64 of relPath — already the hash used for thumb/index files), `name`, `type`, `path` (absolute), `relPath`, `pack`, `ext`, `size`, `score`, `variants`, and when cheap `w,h` / `tris` / `seconds`. Progress (scan, embedding) as NDJSON on stderr. Exit codes: 0 ok, 1 error, 2 bad args, 3 no semantic index / no key, 4 not found.

**Contact sheet.** `--sheet out.png` tiles the top-N thumbs (textures from cache, models via hidden-window render, audio as waveforms) with index numbers burned in; print the path. Single most useful agent feature in the research.

**Manifest.** `export` appends `{id, src, dst, transform, time}` to `<project>/assets-used.json` unless `--no-manifest`.

**Docs.** Ship `SKILL.md` (frontmatter description is the trigger: "search and export textures/models/sounds from the user's local asset library") with 3–4 example invocations and the workflow (search → sheet → inspect → export). `ab --help` prints the same.

**Performance.** Add a scan cache (`%LOCALAPPDATA%\AssetBrowser\scan\<root-hash>.json`, validated by directory mtimes or a full re-walk with `--rescan`) so one-shot `search` on a large library is fast; defer `serve` until that's proven insufficient.

### Suggested order

1. `abcore` static lib + `ab.exe` console target; `Filter()` extraction. (`ab search --json`, `ab packs`)
2. `inspect` + `export` for textures and audio (pure code already exists).
3. Contact sheet + texture `--preview`; model preview via hidden-window device.
4. Metadata cache → size/tri/duration filters.
5. `SKILL.md`, `ab skill`, README section.
6. MCP `serve` wrapper, only if demand shows up.
