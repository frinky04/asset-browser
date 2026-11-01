#include "audio_analysis.h"
#include <algorithm>
#include <cmath>
#include <complex>
#include <numeric>

namespace {

constexpr double kPi = 3.14159265358979323846;

double ToDb(double amplitude) { return amplitude > 1e-6 ? 20.0 * std::log10(amplitude) : -120.0; }
double PowerToDb(double power) { return power > 1e-12 ? 10.0 * std::log10(power) : -120.0; }

std::vector<float> MonoMix(const PcmData& pcm) {
    std::vector<float> mono((size_t)pcm.frames);
    const uint32_t ch = pcm.channels;
    for (uint64_t f = 0; f < pcm.frames; ++f) {
        float s = 0;
        for (uint32_t c = 0; c < ch; ++c) s += pcm.samples[(size_t)f * ch + c];
        mono[(size_t)f] = s / ch;
    }
    return mono;
}

// In-place iterative radix-2 FFT; a.size() must be a power of two.
void Fft(std::vector<std::complex<float>>& a) {
    const size_t n = a.size();
    for (size_t i = 1, j = 0; i < n; ++i) {
        size_t bit = n >> 1;
        for (; j & bit; bit >>= 1) j ^= bit;
        j ^= bit;
        if (i < j) std::swap(a[i], a[j]);
    }
    for (size_t len = 2; len <= n; len <<= 1) {
        double ang = -2.0 * kPi / (double)len;
        std::complex<float> wl((float)std::cos(ang), (float)std::sin(ang));
        for (size_t i = 0; i < n; i += len) {
            std::complex<float> w(1, 0);
            for (size_t k = 0; k < len / 2; ++k) {
                std::complex<float> u = a[i + k], v = a[i + k + len / 2] * w;
                a[i + k] = u + v;
                a[i + k + len / 2] = u - v;
                w *= wl;
            }
        }
    }
}

// Hann-windowed power spectrum, scaled so a full-scale sine peaks at 1.0 (0 dB).
struct Spectrum {
    int n;
    std::vector<float> window;
    float norm = 1;
    std::vector<std::complex<float>> buf;

    explicit Spectrum(int size) : n(size), window(size), buf(size) {
        double sum = 0;
        for (int i = 0; i < n; ++i) { window[i] = (float)(0.5 - 0.5 * std::cos(2 * kPi * i / n)); sum += window[i]; }
        norm = (float)(2.0 / sum);
    }
    int Bins() const { return n / 2 + 1; }
    // Window centred on mono[centre], slid back inside the clip at its ends: padding would
    // draw every clip edge as a broadband click. Clips shorter than a window are zero-padded.
    void Power(const std::vector<float>& mono, int64_t centre, float* out) {
        const int64_t len = (int64_t)mono.size();
        int64_t s0 = std::clamp<int64_t>(centre - n / 2, 0, std::max<int64_t>(0, len - n));
        for (int i = 0; i < n; ++i) {
            int64_t k = s0 + i;
            float v = k < len ? mono[(size_t)k] : 0.0f;
            buf[i] = {v * window[i], 0};
        }
        Fft(buf);
        for (int b = 0; b < Bins(); ++b) {
            float m = std::abs(buf[b]) * norm;
            out[b] = m * m;
        }
    }
};

int FftSizeFor(uint32_t sampleRate) { return sampleRate > 24000 ? 2048 : 1024; }

// ITU-R BS.1770 K-weighting: high shelf then high pass, as two biquads (coefficients for any
// sample rate, derived as in libebur128).
struct Biquad {
    double b0, b1, b2, a1, a2;
    double x1 = 0, x2 = 0, y1 = 0, y2 = 0;
    double Run(double x) {
        double y = b0 * x + b1 * x1 + b2 * x2 - a1 * y1 - a2 * y2;
        x2 = x1; x1 = x; y2 = y1; y1 = y;
        return y;
    }
};

void KWeighting(double rate, Biquad& shelf, Biquad& highPass) {
    double f0 = 1681.974450955533, G = 3.999843853973347, Q = 0.7071752369554196;
    double K = std::tan(kPi * f0 / rate);
    double Vh = std::pow(10.0, G / 20.0), Vb = std::pow(Vh, 0.4996667741545416);
    double a0 = 1.0 + K / Q + K * K;
    shelf = {(Vh + Vb * K / Q + K * K) / a0, 2.0 * (K * K - Vh) / a0, (Vh - Vb * K / Q + K * K) / a0,
             2.0 * (K * K - 1.0) / a0, (1.0 - K / Q + K * K) / a0};
    f0 = 38.13547087602444; Q = 0.5003270373238773;
    K = std::tan(kPi * f0 / rate);
    a0 = 1.0 + K / Q + K * K;
    highPass = {1.0, -2.0, 1.0, 2.0 * (K * K - 1.0) / a0, (1.0 - K / Q + K * K) / a0};
}

void Loudness(const PcmData& pcm, AudioAnalysis& out) {
    const uint32_t ch = pcm.channels, rate = pcm.sampleRate;
    const uint64_t step = rate / 10; // 100 ms sub-blocks; momentary = 4, short-term = 30
    size_t subBlocks = (size_t)(pcm.frames / step);
    std::vector<double> sub(subBlocks, 0.0); // sum over channels of filtered^2, per sub-block
    double total = 0;
    for (uint32_t c = 0; c < ch; ++c) {
        Biquad shelf, highPass;
        KWeighting(rate, shelf, highPass);
        for (uint64_t f = 0; f < pcm.frames; ++f) {
            double y = highPass.Run(shelf.Run(pcm.samples[(size_t)f * ch + c]));
            double y2 = y * y;
            total += y2;
            size_t sb = (size_t)(f / step);
            if (sb < subBlocks) sub[sb] += y2;
        }
    }
    auto lufs = [](double meanSquare) { return meanSquare > 1e-12 ? -0.691 + 10.0 * std::log10(meanSquare) : -120.0; };
    if (subBlocks < 4) { // shorter than one 400 ms block: measure the clip as a whole
        double l = lufs(total / (double)pcm.frames);
        out.lufsIntegrated = out.lufsMomentaryMax = l;
        return;
    }
    std::vector<double> blocks; // momentary loudness, 75% overlap
    for (size_t i = 0; i + 4 <= subBlocks; ++i)
        blocks.push_back(lufs((sub[i] + sub[i + 1] + sub[i + 2] + sub[i + 3]) / (4.0 * step)));
    out.lufsMomentaryMax = *std::max_element(blocks.begin(), blocks.end());
    if (subBlocks >= 30) {
        out.hasShortTerm = true;
        double sum = std::accumulate(sub.begin(), sub.begin() + 30, 0.0);
        double best = sum;
        for (size_t i = 30; i < subBlocks; ++i) { sum += sub[i] - sub[i - 30]; best = std::max(best, sum); }
        out.lufsShortTermMax = lufs(best / (30.0 * step));
    }
    // gating: absolute -70 LUFS, then relative -10 LU below the mean of what passed
    auto gatedMean = [&](double gate) {
        double e = 0; int n = 0;
        for (double l : blocks) if (l > gate) { e += std::pow(10.0, (l + 0.691) / 10.0); ++n; }
        return n ? -0.691 + 10.0 * std::log10(e / n) : -120.0;
    };
    double abs = gatedMean(-70.0);
    out.lufsIntegrated = abs <= -120.0 ? -120.0 : gatedMean(abs - 10.0);
}

void Spectral(const PcmData& pcm, const std::vector<float>& mono, AudioAnalysis& out) {
    const int n = FftSizeFor(pcm.sampleRate);
    Spectrum sp(n);
    const int bins = sp.Bins();
    const double binHz = (double)pcm.sampleRate / n;
    std::vector<float> p(bins);
    std::vector<double> avg(bins, 0.0);
    // Analyse frames that carry sound; silence would pull the average toward the noise floor.
    double gate = std::max(-60.0, out.peakDb - 50.0);
    int lo = std::max(9, (int)(20.0 / binHz)), hi = std::min(bins - 10, (int)(16000.0 / binHz));
    // Tonality: share of a frame's energy in narrow peaks standing 7 dB over their
    // neighbourhood (+-8 bins). Unlike flatness it ignores the overall spectral slope, so a
    // bassy slam still reads as noise and a high whistle as a tone.
    std::vector<double> prefix(bins + 1);
    double tonalSum = 0, weightSum = 0;
    int frames = 0;
    for (int64_t c = n / 2; c < (int64_t)mono.size() + n / 2; c += n / 2) {
        sp.Power(mono, c, p.data());
        double e = 0;
        for (int b = 0; b < bins; ++b) e += p[b];
        if (PowerToDb(e / 1.5) < gate) continue; // Hann power sum ~1.5x the sine's peak bin
        ++frames;
        for (int b = 0; b < bins; ++b) { avg[b] += p[b]; prefix[b + 1] = prefix[b] + p[b]; }
        double total = 0, tonal = 0;
        for (int b = lo; b <= hi; ++b) {
            total += p[b];
            double local = (prefix[b + 9] - prefix[b - 8]) / 17.0;
            if (p[b] > p[b - 1] && p[b] >= p[b + 1] && p[b] > 5.0 * local) tonal += p[b - 1] + p[b] + p[b + 1];
        }
        if (total > 0) { tonalSum += e * std::min(1.0, tonal / total); weightSum += e; }
    }
    if (!frames) return;
    out.tonality = weightSum > 0 ? tonalSum / weightSum : 0;
    double sum = 0, weighted = 0, low = 0, mid = 0, high = 0;
    int best = -1;
    for (int b = 1; b < bins; ++b) {
        double f = b * binHz;
        sum += avg[b]; weighted += f * avg[b];
        (f < 250 ? low : f < 4000 ? mid : high) += avg[b];
        if (f >= 30 && (best < 0 || avg[b] > avg[best])) best = b;
    }
    if (sum <= 0) return;
    out.centroidHz = weighted / sum;
    out.bandLow = low / sum; out.bandMid = mid / sum; out.bandHigh = high / sum;
    if (best > 0) { // parabolic interpolation on dB for a finer peak
        double off = 0;
        if (best + 1 < bins) {
            double l = PowerToDb(avg[best - 1]), c = PowerToDb(avg[best]), r = PowerToDb(avg[best + 1]);
            double d = l - 2 * c + r;
            if (d < 0) off = std::clamp(0.5 * (l - r) / d, -0.5, 0.5);
        }
        out.dominantHz = (best + off) * binHz;
    }
}

// Distinct sounds from a 10 ms RMS envelope: hysteresis regions, split again where the level
// jumps after a dip (rapid hits that ring into each other), then grown into soft attacks/tails.
void Segments(const PcmData& pcm, const std::vector<double>& env, double hop, AudioAnalysis& out) {
    if (env.empty()) return;
    std::vector<double> sorted = env;
    std::sort(sorted.begin(), sorted.end());
    double floor = sorted[sorted.size() / 10];
    double top = sorted.back();
    out.noiseFloorDb = floor;
    if (top < -70) return; // effectively silent
    const int n = (int)env.size();
    // 50 ms power average: noisy material (gravel, mud) has 10 ms blocks that dip 20 dB inside
    // one sound. Regions, growth and dips use this; onsets use the raw envelope.
    std::vector<double> sm(n);
    for (int i = 0; i < n; ++i) {
        double p = 0; int c = 0;
        for (int k = std::max(0, i - 2); k <= std::min(n - 1, i + 2); ++k) { p += std::pow(10.0, env[k] / 10); ++c; }
        sm[i] = 10 * std::log10(p / c);
    }
    std::vector<std::pair<int, int>> regions; // [a, b) in envelope blocks
    if (top - floor < 10) {
        regions.push_back({0, n}); // continuous sound (drone, ambience): one region
    } else {
        // a sound must come within 30 dB of the loudest moment: quieter blips are handling noise
        double on = std::max({floor + 10, top - 30, -65.0}), off = on - 6;
        for (int i = 0; i < n;) {
            if (sm[i] < on) { ++i; continue; }
            int a = i;
            while (a > 0 && sm[a - 1] >= off) --a;
            int b = i;
            while (b < n && sm[b] >= off) ++b;
            if (!regions.empty() && a - regions.back().second < 6) regions.back().second = b; // 60 ms gap
            else regions.push_back({a, b});
            i = b;
        }
        // split at the dip before a sharp rise (rapid hits that ring into each other)
        std::vector<std::pair<int, int>> split;
        for (auto [a, b] : regions) {
            int start = a, lastOnset = a;
            for (int i = a + 3; i < b; ++i) {
                double dip = std::min({env[i - 1], env[i - 2], env[i - 3]});
                if (env[i] - dip < 9 || env[i] < on || i - lastOnset < 8) continue;
                int cut = (int)(std::min_element(env.begin() + lastOnset + 1, env.begin() + i) - env.begin());
                if (cut - start >= 3) { split.push_back({start, cut}); start = cut; }
                lastOnset = i;
            }
            split.push_back({start, b});
        }
        regions = std::move(split);
        // grow into quiet attacks and decays, never into a neighbour, and stop where the level
        // climbs 6 dB off its lowest point: that is the next (quieter) event, not this one's tail
        double grow = std::max(floor + 6, top - 60);
        for (size_t k = 0; k < regions.size(); ++k) {
            int lim0 = k ? regions[k - 1].second : 0;
            int lim1 = k + 1 < regions.size() ? regions[k + 1].first : n;
            int& a = regions[k].first;
            for (double low = sm[a]; a > lim0 && sm[a - 1] > grow && sm[a - 1] < low + 6; --a) low = std::min(low, sm[a - 1]);
            int& b = regions[k].second;
            for (double low = sm[b - 1]; b < lim1 && sm[b] > grow && sm[b] < low + 6; ++b) low = std::min(low, sm[b]);
        }
        // Neighbours that still touch never fell back to the floor between them. They are one
        // sound when the second is 10 dB under the first (the squelch after a footstep) or the
        // dip between them is shallow (heel then toe); separate hits drop well below the quieter
        // of the two peaks.
        auto peakOf = [&](std::pair<int, int> r) { return *std::max_element(env.begin() + r.first, env.begin() + r.second); };
        std::vector<std::pair<int, int>> merged;
        for (auto r : regions) {
            if (!merged.empty() && merged.back().second >= r.first && r.second > r.first) {
                double prev = peakOf(merged.back()), cur = peakOf(r);
                int d0 = std::max(merged.back().first, r.first - 5), d1 = std::min(r.second, r.first + 5);
                double dip = *std::min_element(sm.begin() + d0, sm.begin() + d1);
                if (cur < prev - 10 || std::min(prev, cur) - dip < 12) { merged.back().second = r.second; continue; }
            }
            merged.push_back(r);
        }
        regions = std::move(merged);
        regions.erase(std::remove_if(regions.begin(), regions.end(),
                                     [](auto& r) { return r.second - r.first < 2; }), regions.end());
    }
    const uint32_t ch = pcm.channels;
    const uint64_t hopFrames = (uint64_t)(hop * pcm.sampleRate);
    for (auto [a, b] : regions) {
        uint64_t f0 = std::min(pcm.frames, (uint64_t)a * hopFrames);
        uint64_t f1 = b >= n ? pcm.frames : std::min(pcm.frames, (uint64_t)b * hopFrames);
        double peak = 0, sq = 0;
        for (size_t i = (size_t)f0 * ch; i < (size_t)f1 * ch; ++i) {
            double v = pcm.samples[i];
            peak = std::max(peak, std::fabs(v)); sq += v * v;
        }
        AudioSegment s;
        s.start = (double)f0 / pcm.sampleRate;
        s.end = (double)f1 / pcm.sampleRate;
        s.peakDb = ToDb(peak);
        s.rmsDb = f1 > f0 ? ToDb(std::sqrt(sq / ((f1 - f0) * ch))) : -120;
        out.segments.push_back(s);
    }
}

} // namespace

PcmData SlicePcm(const PcmData& pcm, uint64_t f0, uint64_t f1) {
    PcmData out;
    f0 = std::min(f0, pcm.frames); f1 = std::min(f1, pcm.frames);
    out.channels = pcm.channels; out.sampleRate = pcm.sampleRate;
    out.frames = f1 > f0 ? f1 - f0 : 0;
    out.samples.assign(pcm.samples.begin() + (size_t)f0 * pcm.channels, pcm.samples.begin() + (size_t)f1 * pcm.channels);
    out.valid = pcm.valid && out.frames > 0;
    return out;
}

const char* SpectralCharacter(double tonality) {
    return tonality >= 0.5 ? "tonal" : tonality >= 0.2 ? "mixed" : "noisy";
}

AudioAnalysis AnalyzeAudio(const PcmData& pcm) {
    AudioAnalysis out;
    if (!pcm.valid || pcm.frames == 0 || pcm.channels == 0 || pcm.sampleRate == 0) return out;
    const uint32_t ch = pcm.channels;
    const double rate = pcm.sampleRate;
    out.seconds = pcm.frames / rate;

    double peak = 0, sq = 0;
    for (float v : pcm.samples) {
        double a = std::fabs(v);
        peak = std::max(peak, a); sq += (double)v * v;
        if (a >= 0.999) ++out.clippedSamples;
    }
    out.peakDb = ToDb(peak);
    out.rmsDb = ToDb(std::sqrt(sq / pcm.samples.size()));
    out.crestDb = out.peakDb - out.rmsDb;

    std::vector<float> mono = MonoMix(pcm);
    out.dcOffset = std::accumulate(mono.begin(), mono.end(), 0.0) / mono.size();

    // same -50 dBFS rule as --trim-silence
    auto loud = [&](uint64_t f) {
        for (uint32_t c = 0; c < ch; ++c) if (std::fabs(pcm.samples[(size_t)f * ch + c]) > 0.00316f) return true;
        return false;
    };
    uint64_t a = 0, b = pcm.frames;
    while (a < b && !loud(a)) ++a;
    while (b > a && !loud(b - 1)) --b;
    out.leadingSilence = a / rate;
    out.trailingSilence = (pcm.frames - b) / rate;

    if (ch >= 2) {
        double lr = 0, ll = 0, rr = 0;
        for (uint64_t f = 0; f < pcm.frames; ++f) {
            double l = pcm.samples[(size_t)f * ch], r = pcm.samples[(size_t)f * ch + 1];
            lr += l * r; ll += l * l; rr += r * r;
        }
        out.stereo = true;
        out.stereoCorrelation = (ll > 0 && rr > 0) ? lr / std::sqrt(ll * rr) : 1.0;
    }

    uint64_t edge = std::min<uint64_t>(pcm.frames / 2, (uint64_t)(0.05 * rate));
    if (edge > 0) {
        double s0 = 0, s1 = 0;
        for (size_t i = 0; i < (size_t)edge * ch; ++i) s0 += (double)pcm.samples[i] * pcm.samples[i];
        for (size_t i = (size_t)(pcm.frames - edge) * ch; i < pcm.samples.size(); ++i) s1 += (double)pcm.samples[i] * pcm.samples[i];
        out.loopLevelJumpDb = std::fabs(ToDb(std::sqrt(s1 / (edge * ch))) - ToDb(std::sqrt(s0 / (edge * ch))));
        for (uint32_t c = 0; c < ch; ++c)
            out.loopSampleJump = std::max(out.loopSampleJump,
                (double)std::fabs(pcm.samples[(size_t)(pcm.frames - 1) * ch + c] - pcm.samples[c]));
    }

    Loudness(pcm, out);
    Spectral(pcm, mono, out);

    const double hop = 0.01;
    const uint64_t hopFrames = std::max<uint64_t>(1, (uint64_t)(hop * rate));
    std::vector<double> env;
    for (uint64_t f = 0; f < pcm.frames; f += hopFrames) {
        uint64_t e = std::min(pcm.frames, f + hopFrames);
        double s = 0;
        for (size_t i = (size_t)f * ch; i < (size_t)e * ch; ++i) s += (double)pcm.samples[i] * pcm.samples[i];
        env.push_back(ToDb(std::sqrt(s / ((e - f) * ch))));
    }
    Segments(pcm, env, hopFrames / rate, out);
    return out;
}

std::vector<float> SpectrogramDb(const PcmData& pcm, int cols, int rows, float loHz, float hiHz) {
    std::vector<float> out((size_t)cols * rows, -120.0f);
    if (!pcm.valid || pcm.frames == 0 || cols <= 0 || rows <= 0) return out;
    std::vector<float> mono = MonoMix(pcm);
    const int n = FftSizeFor(pcm.sampleRate);
    Spectrum sp(n);
    const int bins = sp.Bins();
    const double binHz = (double)pcm.sampleRate / n;
    hiHz = std::min(hiHz, pcm.sampleRate / 2.0f);
    // band edges per row in fractional bins (row 0 = top)
    std::vector<double> edge(rows + 1);
    for (int r = 0; r <= rows; ++r)
        edge[r] = loHz * std::pow((double)hiHz / loHz, (double)(rows - r) / rows) / binHz;
    std::vector<float> p(bins), acc(bins);
    for (int x = 0; x < cols; ++x) {
        double s0 = (double)pcm.frames * x / cols, s1 = (double)pcm.frames * (x + 1) / cols;
        int k = std::clamp((int)((s1 - s0) / (n / 2)), 1, 4);
        std::fill(acc.begin(), acc.end(), 0.0f);
        for (int w = 0; w < k; ++w) {
            sp.Power(mono, (int64_t)(s0 + (s1 - s0) * (w + 0.5) / k), p.data());
            for (int b = 0; b < bins; ++b) acc[b] += p[b] / k;
        }
        for (int r = 0; r < rows; ++r) {
            double top = edge[r], bot = edge[r + 1];
            float v;
            if (top - bot < 1.0) { // narrower than a bin: interpolate at the band centre
                double c = std::sqrt(top * bot);
                int b0 = std::clamp((int)c, 0, bins - 2);
                double t = std::clamp(c - b0, 0.0, 1.0);
                v = (float)(acc[b0] * (1 - t) + acc[b0 + 1] * t);
            } else { // keep the strongest bin so tones stay visible in wide bands
                v = 0;
                for (int b = std::max(0, (int)std::ceil(bot)); b <= std::min(bins - 1, (int)top); ++b) v = std::max(v, acc[b]);
            }
            out[(size_t)r * cols + x] = (float)PowerToDb(v);
        }
    }
    return out;
}

void EnvelopeDb(const PcmData& pcm, int cols, std::vector<float>& peakDb, std::vector<float>& rmsDb) {
    peakDb.assign(cols, -120.0f);
    rmsDb.assign(cols, -120.0f);
    if (!pcm.valid || pcm.frames == 0) return;
    const uint32_t ch = pcm.channels;
    for (int x = 0; x < cols; ++x) {
        uint64_t f0 = pcm.frames * x / cols, f1 = pcm.frames * (x + 1) / cols;
        if (f1 <= f0) f1 = std::min(pcm.frames, f0 + 1);
        double peak = 0, sq = 0;
        for (size_t i = (size_t)f0 * ch; i < (size_t)f1 * ch; ++i) {
            double v = pcm.samples[i];
            peak = std::max(peak, std::fabs(v)); sq += v * v;
        }
        peakDb[x] = (float)ToDb(peak);
        rmsDb[x] = (float)ToDb(std::sqrt(sq / ((f1 - f0) * ch)));
    }
}
