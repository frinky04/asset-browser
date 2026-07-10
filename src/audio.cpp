#include "audio.h"
#include "util.h"
#include <cmath>
#include <cstring>

// stb_vorbis is compiled as C in its own TU (stb_vorbis_impl.c)
extern "C" int stb_vorbis_decode_memory(const unsigned char* mem, int len,
                                        int* channels, int* sample_rate, short** output);

namespace {

PcmData DecodeOgg(const std::wstring& path) {
    PcmData out;
    std::vector<uint8_t> bytes;
    if (!ReadFileBytes(path, bytes) || bytes.empty()) return out;
    int channels = 0, rate = 0;
    short* samples = nullptr;
    int frames = stb_vorbis_decode_memory(bytes.data(), (int)bytes.size(), &channels, &rate, &samples);
    if (frames <= 0 || !samples) return out;
    out.channels = (uint32_t)channels;
    out.sampleRate = (uint32_t)rate;
    out.frames = (uint64_t)frames;
    out.samples.resize((size_t)frames * channels);
    for (size_t i = 0; i < out.samples.size(); ++i)
        out.samples[i] = samples[i] / 32768.0f;
    free(samples);
    out.valid = true;
    return out;
}

PcmData DecodeWithMiniaudio(const std::wstring& path) {
    PcmData out;
    ma_decoder_config cfg = ma_decoder_config_init(ma_format_f32, 0, 0);
    ma_decoder dec;
    if (ma_decoder_init_file_w(path.c_str(), &cfg, &dec) != MA_SUCCESS) return out;
    out.channels = dec.outputChannels;
    out.sampleRate = dec.outputSampleRate;
    std::vector<float> buf;
    float chunk[4096];
    ma_uint64 framesPerChunk = 4096 / (dec.outputChannels ? dec.outputChannels : 1);
    for (;;) {
        ma_uint64 read = 0;
        ma_result r = ma_decoder_read_pcm_frames(&dec, chunk, framesPerChunk, &read);
        if (read > 0)
            buf.insert(buf.end(), chunk, chunk + (size_t)read * dec.outputChannels);
        if (r != MA_SUCCESS || read < framesPerChunk) break;
    }
    ma_decoder_uninit(&dec);
    if (buf.empty() || out.channels == 0) return out;
    out.samples = std::move(buf);
    out.frames = out.samples.size() / out.channels;
    out.valid = true;
    return out;
}

} // namespace

PcmData DecodeAudioFile(const std::wstring& path) {
    std::wstring lower = ToLowerAsciiW(path);
    if (lower.size() >= 4 && lower.compare(lower.size() - 4, 4, L".ogg") == 0)
        return DecodeOgg(path);
    return DecodeWithMiniaudio(path);
}

std::vector<uint8_t> RenderWaveformThumb(const PcmData& pcm, int w, int h) {
    std::vector<uint8_t> px((size_t)w * h * 4);
    auto put = [&](int x, int y, uint8_t r, uint8_t g, uint8_t b) {
        if (x < 0 || y < 0 || x >= w || y >= h) return;
        size_t o = ((size_t)y * w + x) * 4;
        px[o] = r; px[o + 1] = g; px[o + 2] = b; px[o + 3] = 255;
    };
    // background (matches tile background in the dark theme)
    for (int y = 0; y < h; ++y)
        for (int x = 0; x < w; ++x)
            put(x, y, 26, 26, 29);

    if (!pcm.valid || pcm.frames == 0) return px;

    int mid = h / 2;
    float amp = h * 0.42f;
    for (int x = 0; x < w; ++x) {
        uint64_t f0 = pcm.frames * x / w;
        uint64_t f1 = pcm.frames * (x + 1) / w;
        if (f1 <= f0) f1 = f0 + 1;
        float mn = 1e9f, mx = -1e9f;
        for (uint64_t f = f0; f < f1 && f < pcm.frames; ++f) {
            float s = 0;
            for (uint32_t c = 0; c < pcm.channels; ++c)
                s += pcm.samples[(size_t)f * pcm.channels + c];
            s /= (float)pcm.channels;
            mn = (std::min)(mn, s);
            mx = (std::max)(mx, s);
        }
        if (mn > mx) { mn = mx = 0; }
        int y0 = mid - (int)(mx * amp);
        int y1 = mid - (int)(mn * amp);
        if (y1 == y0) y1 = y0 + 1;
        for (int y = y0; y <= y1; ++y)
            put(x, y, 196, 198, 206);
    }
    // faint center line
    for (int x = 0; x < w; x += 2)
        put(x, mid, 82, 84, 92);
    return px;
}

bool WriteWav16(const std::wstring& path, const PcmData& pcm, uint64_t f0, uint64_t f1) {
    if (!pcm.valid || pcm.channels == 0) return false;
    f0 = (std::min)(f0, pcm.frames);
    f1 = (std::min)(f1, pcm.frames);
    if (f1 <= f0) return false;

    uint64_t frames = f1 - f0;
    uint32_t dataBytes = (uint32_t)(frames * pcm.channels * 2);
    uint16_t blockAlign = (uint16_t)(pcm.channels * 2);
    uint32_t byteRate = pcm.sampleRate * blockAlign;

    FILE* f = _wfopen(path.c_str(), L"wb");
    if (!f) return false;
    uint32_t riffSize = 36 + dataBytes;
    uint16_t fmt16 = 1 /*PCM*/, bits = 16;
    uint16_t channels = (uint16_t)pcm.channels;
    uint32_t fmtSize = 16;
    fwrite("RIFF", 1, 4, f); fwrite(&riffSize, 4, 1, f); fwrite("WAVE", 1, 4, f);
    fwrite("fmt ", 1, 4, f); fwrite(&fmtSize, 4, 1, f);
    fwrite(&fmt16, 2, 1, f); fwrite(&channels, 2, 1, f);
    fwrite(&pcm.sampleRate, 4, 1, f); fwrite(&byteRate, 4, 1, f);
    fwrite(&blockAlign, 2, 1, f); fwrite(&bits, 2, 1, f);
    fwrite("data", 1, 4, f); fwrite(&dataBytes, 4, 1, f);

    std::vector<int16_t> buf(4096);
    size_t total = (size_t)frames * pcm.channels;
    const float* src = pcm.samples.data() + (size_t)f0 * pcm.channels;
    size_t written = 0;
    while (written < total) {
        size_t n = (std::min)(buf.size(), total - written);
        for (size_t i = 0; i < n; ++i) {
            float s = src[written + i];
            s = (std::max)(-1.0f, (std::min)(1.0f, s));
            buf[i] = (int16_t)(s * 32767.0f);
        }
        fwrite(buf.data(), 2, n, f);
        written += n;
    }
    fclose(f);
    return true;
}

// ---------------------------------------------------------------- AudioSystem

bool AudioSystem::Init() {
    engineOk = (ma_engine_init(nullptr, &engine) == MA_SUCCESS);
    return engineOk;
}

void AudioSystem::Shutdown() {
    ReleaseSound();
    if (engineOk) ma_engine_uninit(&engine);
    engineOk = false;
}

void AudioSystem::ReleaseSound() {
    if (hasSound) {
        ma_sound_uninit(&sound);
        ma_audio_buffer_uninit(&buffer);
        hasSound = false;
    }
    pcm = PcmData{};
    playingIndex = -1;
    stopAtFrame = 0;
}

void AudioSystem::Toggle(int assetIndex, const std::wstring& path) {
    if (!engineOk) return;
    if (playingIndex == assetIndex) { Stop(); return; }
    PlayRange(assetIndex, path, 0.0f, -1.0f);
}

void AudioSystem::PlayRange(int assetIndex, const std::wstring& path, float startSec, float endSec) {
    if (!engineOk) return;
    ReleaseSound();

    pcm = DecodeAudioFile(path);
    if (!pcm.valid) return;

    ma_audio_buffer_config cfg = ma_audio_buffer_config_init(
        ma_format_f32, pcm.channels, pcm.frames, pcm.samples.data(), nullptr);
    cfg.sampleRate = pcm.sampleRate;
    if (ma_audio_buffer_init(&cfg, &buffer) != MA_SUCCESS) { pcm = PcmData{}; return; }
    if (ma_sound_init_from_data_source(&engine, &buffer, 0, nullptr, &sound) != MA_SUCCESS) {
        ma_audio_buffer_uninit(&buffer);
        pcm = PcmData{};
        return;
    }
    hasSound = true;
    if (startSec > 0.0f) {
        ma_uint64 frame = (ma_uint64)((double)startSec * pcm.sampleRate);
        if (frame < pcm.frames) ma_sound_seek_to_pcm_frame(&sound, frame);
    }
    stopAtFrame = 0;
    if (endSec >= 0.0f) {
        ma_uint64 frame = (ma_uint64)((double)endSec * pcm.sampleRate);
        stopAtFrame = (std::min)((ma_uint64)pcm.frames, frame);
    }
    ma_sound_start(&sound);
    playingIndex = assetIndex;
}

void AudioSystem::Stop() {
    ReleaseSound();
}

bool AudioSystem::IsPlaying(int assetIndex) const {
    return playingIndex == assetIndex;
}

bool AudioSystem::AnyPlaying() const { return playingIndex >= 0; }

float AudioSystem::LengthSeconds() const {
    if (!hasSound || pcm.sampleRate == 0) return 0;
    return (float)pcm.frames / (float)pcm.sampleRate;
}

float AudioSystem::CursorSeconds() {
    if (!hasSound) return 0;
    float t = 0;
    ma_sound_get_cursor_in_seconds(&sound, &t);
    return t;
}

void AudioSystem::SeekSeconds(float t) {
    if (!hasSound || pcm.sampleRate == 0) return;
    ma_uint64 frame = (ma_uint64)((double)t * pcm.sampleRate);
    if (frame >= pcm.frames) frame = pcm.frames ? pcm.frames - 1 : 0;
    ma_sound_seek_to_pcm_frame(&sound, frame);
}

void AudioSystem::Update() {
    if (!hasSound) return;
    if (ma_sound_at_end(&sound)) { ReleaseSound(); return; }
    if (stopAtFrame > 0) {
        ma_uint64 cur = 0;
        if (ma_sound_get_cursor_in_pcm_frames(&sound, &cur) == MA_SUCCESS && cur >= stopAtFrame)
            ReleaseSound();
    }
}
