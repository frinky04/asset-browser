# Handoff: Sound-effect embedding search via OpenRouter (google/gemini-embedding-2)

Date: 2026-08-22
Workspace: `C:\Users\cuddl\Documents\Dev\Research\embed textures`
Companion doc (textures/images, app architecture recs): `embed-textures-handoff.md` — read it first; everything there about BYOK, index storage, model versioning, hybrid search applies here too and is not repeated.

## Context
Prototype for the user's mass asset browser app: embed an SFX library so users can search by text or by example sound. Model fixed by the user: `google/gemini-embedding-2` via OpenRouter. Key in env var `OPENROUTER_API_KEY` (never print).

## Artifacts
- `embed_audio.py` – `to_mp3()` (ffmpeg -> 32 kbps mono mp3, optional `max_s` truncation), `embed_audio(src, max_s, text, idx)`, `name_text()` (folder + cleaned filename). `python embed_audio.py` -> `audio.json`; `--hybrid` -> `audio_hybrid.json`. Reuses `call()`/`MODEL` from `embed.py`.
- `audio.json`, `audio_hybrid.json` – 126 × 3072-dim vectors keyed by path.
- `sounds-inventory.txt` – codec/sample-rate/channels/duration per file.
- `test-sounds/` – 126 files (119 ogg Vorbis, 7 wav), 44.1 kHz stereo, 19.3 min total, avg 9.2 s, 37 MB; 8 category folders (Buttons & Levers, Cinematic, Doors & Gates, Entities, Machines, Misc, Monsters, UI).
- ffmpeg/ffprobe are on PATH (winget yt-dlp.FFmpeg).

## Verified API facts
- Endpoint/model same as images. Audio is passed as a data URL inside the `image_url` content part: `{"type":"image_url","image_url":{"url":"data:audio/mpeg;base64,..."}}`. `input_audio` also works but only with a data URL in `data`; `file` parts are rejected (400).
- Accepted MIME tested: audio/ogg, audio/wav, audio/mpeg, audio/flac, audio/mp4 (aac).
- Pricing: **25 tokens per second of audio, $0.000162/s**, exactly linear (1 s=25, 60 s=1500 tokens). Independent of container, codec, bitrate, sample rate, channels (tested ogg, wav 44.1k stereo/16k mono/8k mono, mp3 128k/32k/16k, flac, aac 64k — identical tokens).
- Audio tokens cost ~14× image tokens: 10 s clip ≈ $0.0016 vs $0.00012 per image.
- Latency 1.4–5 s/request, dominated by upload size; 8 threads no rate limiting. Full library: 18 s, $0.19 (29k tokens).
- Scaling: $0.58 per hour of audio; 1k clips @9 s ≈ $1.46 / 2.5 min; 10k ≈ $14.6 / 25 min; 100k ≈ $146 / 4 h (8 threads).

## Retrieval findings
- Audio→audio: excellent (siblings of the same sound come back at 0.8 sim).
- Text→audio, audio-only index: good for distinctive sounds (monster, scream, drone), weak for short UI/button/machine sounds.
- Hybrid (folder + filename text + audio in one request, +4% tokens): fixes nearly all text-query failures; scores 0.6 → 0.75. Same conclusion as images: always embed metadata with the media. Store both vectors if audio→audio precision matters.
- Truncation test (12 clips of 14–49 s, query = first N s vs full-length index): 3 s → 1/12 top-1; 5 s → 4/12; **10 s → 10/12 top-1, 11/12 top-3, sim 0.96**.

## Recommendations for the app
1. Transcode to 32 kbps mono MP3 (or 16 kHz mono) before upload — same cost, ~40× smaller than wav, fastest latency.
2. Index only the first 10–15 s (or a centred 10 s window for loops/ambiences); caps cost at ~$0.002/clip. For long ambiences/music consider 2–3 windows averaged or stored separately.
3. Hybrid text+audio vector as the default index; optional audio-only vector for example-based search.
4. Show the user an exact upfront estimate: `total_seconds_indexed * 25 tokens * price`.
5. Everything else (dimensions 768, model-id versioning, incremental indexing by hash, retry/backoff, local fallback) — see the textures handoff. For a local audio fallback, CLAP (laion/clap-htsat) is the SigLIP equivalent for audio↔text.

## Open items
- `dimensions` param via OpenRouter untested.
- Windowed/averaged embeddings for long clips not tested.
- Music (vs SFX) retrieval quality not tested.
- Text-only embedding for clips with no preview not tested.

## Suggested skills
- `code-review` after implementing the audio embedding module in the app repo.
- `simplify` for cleanup after implementation.
- `claude-api` only if Anthropic models get involved; skip for Gemini/OpenRouter work.
