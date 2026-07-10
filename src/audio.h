#pragma once
#include <string>
#include <vector>
#include <cstdint>
#include "miniaudio.h"

struct PcmData {
    uint32_t channels = 0, sampleRate = 0;
    uint64_t frames = 0;
    std::vector<float> samples; // interleaved f32
    bool valid = false;
};

// Thread-safe (used by thumbnail workers and the playback path).
PcmData DecodeAudioFile(const std::wstring& path);

// Renders a min/max envelope waveform into an RGBA thumbnail.
std::vector<uint8_t> RenderWaveformThumb(const PcmData& pcm, int w, int h);

// Writes frames [f0, f1) as a 16-bit PCM WAV file.
bool WriteWav16(const std::wstring& path, const PcmData& pcm, uint64_t f0, uint64_t f1);

struct AudioSystem {
    bool Init();
    void Shutdown();

    // Toggle playback of the given asset; starting a new one stops the old.
    void Toggle(int assetIndex, const std::wstring& path);
    // Play only [startSec, endSec) of the file (auto-stops at the end mark).
    void PlayRange(int assetIndex, const std::wstring& path, float startSec, float endSec);
    void Stop();

    bool IsPlaying(int assetIndex) const;
    bool AnyPlaying() const;
    int  PlayingIndex() const { return playingIndex; }

    float LengthSeconds() const;
    float CursorSeconds();
    void  SeekSeconds(float t);
    void  Update(); // clear playing state when the sound reaches its end

private:
    void ReleaseSound();

    ma_engine engine{};
    bool engineOk = false;
    ma_audio_buffer buffer{};
    ma_sound sound{};
    bool hasSound = false;
    PcmData pcm;
    int playingIndex = -1;
    ma_uint64 stopAtFrame = 0; // 0 = play to end
};
