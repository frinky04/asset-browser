#include "app.h"
#include "util.h"
#include <algorithm>
#include <cctype>

namespace {

// Compact fuzzy matcher in the spirit of fts_fuzzy_match: sequential character
// match with bonuses for word starts / separators / consecutive runs.
bool FuzzyMatch(const char* pattern, const char* str, int& outScore) {
    if (!*pattern) { outScore = 0; return true; }

    int score = 0;
    int consecutive = 0;
    int unmatchedLeading = 0;
    bool anyMatched = false;
    const char* p = pattern;
    char prev = 0;

    for (const char* s = str; *s; ++s) {
        if (*p && std::tolower((unsigned char)*s) == std::tolower((unsigned char)*p)) {
            int bonus = 0;
            bool isSep = (prev == '_' || prev == '-' || prev == ' ' || prev == '/' ||
                          prev == '\\' || prev == '.');
            if (prev == 0 || isSep) bonus += 30;                       // word start
            else if (std::isupper((unsigned char)*s) && std::islower((unsigned char)prev))
                bonus += 30;                                            // camelCase
            bonus += consecutive * 15;                                  // run bonus
            if (!anyMatched)
                bonus -= (std::min)(unmatchedLeading, 3) * 5;           // leading gap penalty
            score += 10 + bonus;
            ++consecutive;
            anyMatched = true;
            ++p;
        } else {
            consecutive = 0;
            if (!anyMatched) ++unmatchedLeading;
            else score -= 1;                                            // gap penalty
        }
        prev = *s;
    }
    if (*p) return false; // not all pattern chars matched
    outScore = score;
    return true;
}

} // namespace

void App::RebuildFilterIfDirty() {
    if (!filterDirty) return;
    filterDirty = false;

    filtered.clear();
    filtered.reserve(db.entries.size());
    bool hasQuery = searchBuf[0] != 0;

    std::vector<std::pair<int, int>> scored; // (score, index)
    if (hasQuery) scored.reserve(1024);

    bool anyType = typeSel[0] || typeSel[1] || typeSel[2];
    for (int i = 0; i < (int)db.entries.size(); ++i) {
        const AssetEntry& e = db.entries[i];
        if (anyType && !typeSel[(int)e.type]) continue;
        if (!selectedPacks.empty() && !selectedPacks.count(e.pack)) continue;

        if (hasQuery) {
            int score = 0;
            if (!FuzzyMatch(searchBuf, e.relPath.c_str(), score)) continue;
            // filename matches beat directory-only matches
            int nameScore = 0;
            if (FuzzyMatch(searchBuf, e.name.c_str(), nameScore))
                score += nameScore + 40;
            scored.emplace_back(score, i);
        } else {
            filtered.push_back(i);
        }
    }

    if (hasQuery) {
        std::stable_sort(scored.begin(), scored.end(),
                         [](const auto& a, const auto& b) { return a.first > b.first; });
        for (auto& [s, i] : scored) filtered.push_back(i);
    }
}
