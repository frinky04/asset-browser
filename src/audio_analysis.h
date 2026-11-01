#pragma once
// Measurements for one decoded clip (loudness, spectrum, distinct sounds) and the
// per-pixel data the CLI's waveform + spectrogram pictures are drawn from.
#include "audio.h"
#include <cstdint>
#include <vector>

// One distinct sound inside a clip (a footstep in a take of eight, the hit in a long tail).
struct AudioSegment {
    double start = 0, end = 0;          // seconds
    double peakDb = -120, rmsDb = -120; // dBFS
};

struct AudioAnalysis {
    double seconds = 0;
    double peakDb = -120;            // sample peak, dBFS
    double rmsDb = -120;
    double crestDb = 0;              // peak - rms: high = punchy/transient, low = dense/compressed
    double lufsIntegrated = -120;    // ITU-R BS.1770 (gated); clips < 0.4 s are measured as one block
    double lufsMomentaryMax = -120;  // loudest 400 ms
    double lufsShortTermMax = -120;  // loudest 3 s; only when the clip is >= 3 s
    bool hasShortTerm = false;
    double noiseFloorDb = -120;      // 10th percentile of 10 ms RMS
    double dcOffset = 0;
    uint64_t clippedSamples = 0;     // |s| >= 0.999
    double leadingSilence = 0, trailingSilence = 0; // seconds below -50 dBFS
    double centroidHz = 0;           // brightness
    double dominantHz = 0;           // strongest frequency in the average spectrum
    double tonality = 0;             // share of energy in narrow spectral peaks: 0 noise .. 1 pure tones
    double bandLow = 0, bandMid = 0, bandHigh = 0; // energy share <250 Hz, 250-4k, >4k
    bool stereo = false;
    double stereoCorrelation = 1;    // 1 = identical channels, 0 = unrelated, <0 = out of phase
    double loopLevelJumpDb = 0;      // |RMS(last 50 ms) - RMS(first 50 ms)|
    double loopSampleJump = 0;       // largest |last sample - first sample| over channels
    std::vector<AudioSegment> segments;
};

AudioAnalysis AnalyzeAudio(const PcmData& pcm);

// Frames [f0, f1) as a standalone clip.
PcmData SlicePcm(const PcmData& pcm, uint64_t f0, uint64_t f1);

// Short label for tonality: "tonal", "mixed" or "noisy".
const char* SpectralCharacter(double tonality);

// Power spectrogram in dB (0 dB = full-scale sine), `cols` x `rows`, row-major with row 0 the
// highest band. Rows are log-spaced from loHz to hiHz; each column averages up to four windows
// across its time span so long clips do not alias.
std::vector<float> SpectrogramDb(const PcmData& pcm, int cols, int rows, float loHz, float hiHz);

// Per-column peak (max |s| over channels) and RMS in dBFS.
void EnvelopeDb(const PcmData& pcm, int cols, std::vector<float>& peakDb, std::vector<float>& rmsDb);
