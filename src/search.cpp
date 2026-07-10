#include "app.h"
#include "search.h"
#include "util.h"
#include <algorithm>
#include <charconv>
#include <chrono>
#include <cctype>
#include <string_view>

namespace {

std::vector<std::string> Split(std::string_view value, char separator) {
    std::vector<std::string> result;
    for (size_t start = 0; start <= value.size();) {
        size_t end = value.find(separator, start);
        if (end == std::string_view::npos) end = value.size();
        if (end > start) result.emplace_back(value.substr(start, end - start));
        start = end + 1;
    }
    return result;
}

bool ParseQuantity(std::string_view text, uint64_t& result, bool age) {
    if (text.empty()) return false;
    size_t digits = 0;
    while (digits < text.size() && std::isdigit((unsigned char)text[digits])) ++digits;
    if (!digits) return false;
    uint64_t value = 0;
    auto parsed = std::from_chars(text.data(), text.data() + digits, value);
    if (parsed.ec != std::errc()) return false;
    std::string suffix(text.substr(digits));
    uint64_t multiplier = 1;
    if (age) {
        if (suffix == "d") multiplier = 24ull * 60 * 60;
        else if (suffix == "w") multiplier = 7ull * 24 * 60 * 60;
        else if (suffix == "m") multiplier = 30ull * 24 * 60 * 60;
        else if (suffix == "y") multiplier = 365ull * 24 * 60 * 60;
        else return false;
    } else {
        if (suffix.empty() || suffix == "b") multiplier = 1;
        else if (suffix == "kb") multiplier = 1024ull;
        else if (suffix == "mb") multiplier = 1024ull * 1024;
        else if (suffix == "gb") multiplier = 1024ull * 1024 * 1024;
        else return false;
    }
    if (value > UINT64_MAX / multiplier) return false;
    result = value * multiplier;
    return true;
}

ParsedQuery ParseQuery(std::string_view input) {
    ParsedQuery query;
    for (size_t pos = 0; pos < input.size();) {
        while (pos < input.size() && std::isspace((unsigned char)input[pos])) ++pos;
        if (pos == input.size()) break;

        bool quoted = input[pos] == '"';
        size_t start = pos + (quoted ? 1 : 0);
        size_t end = quoted ? input.find('"', start) : input.find_first_of(" \t\r\n", start);
        if (end == std::string_view::npos) end = input.size();
        std::string token = ToLowerAscii(std::string(input.substr(start, end - start)));
        pos = end + (quoted && end < input.size() ? 1 : 0);
        if (token.empty()) continue;

        SearchTerm term;
        if (quoted) {
            term.kind = SearchTermKind::Exact;
            term.value = std::move(token);
        } else if (token[0] == '-' && token.size() > 1) {
            term.kind = SearchTermKind::Negated;
            term.value = token.substr(1);
        } else {
            size_t colon = token.find(':');
            std::string field = colon == std::string::npos ? "" : token.substr(0, colon);
            std::string value = colon == std::string::npos ? token : token.substr(colon + 1);
            if (field == "ext" && !value.empty()) {
                term.kind = SearchTermKind::Extension;
                term.values = Split(value, ',');
            } else if (field == "type" && !value.empty()) {
                term.kind = SearchTermKind::Type;
                term.values = Split(value, '|');
            } else if ((field == "pack" || field == "path") && !value.empty()) {
                term.kind = field == "pack" ? SearchTermKind::Pack : SearchTermKind::Path;
                term.value = std::move(value);
            } else if ((field == "size" || field == "age") && value.size() > 1 &&
                       (value[0] == '<' || value[0] == '>') &&
                       ParseQuantity(std::string_view(value).substr(1), term.number, field == "age")) {
                term.kind = field == "size" ? SearchTermKind::Size : SearchTermKind::Age;
                term.comparison = value[0];
            } else {
                term.kind = SearchTermKind::Fuzzy;
                term.value = std::move(token); // malformed filters remain searchable text
            }
        }
        query.terms.push_back(std::move(term));
    }
    return query;
}

bool FuzzyMatch(std::string_view pattern, std::string_view lower, int& outScore,
                std::vector<int>* positions = nullptr) {
    int score = 0, consecutive = 0, unmatchedLeading = 0;
    bool anyMatched = false;
    size_t p = 0;
    char prev = 0;
    for (size_t i = 0; i < lower.size(); ++i) {
        char c = lower[i];
        if (p < pattern.size() && c == pattern[p]) {
            bool isSep = prev == '_' || prev == '-' || prev == ' ' || prev == '/' || prev == '\\' || prev == '.';
            int bonus = (prev == 0 || isSep) ? 30 : 0;
            bonus += consecutive * 15;
            if (!anyMatched) bonus -= (std::min)(unmatchedLeading, 3) * 5;
            score += 10 + bonus;
            ++consecutive;
            anyMatched = true;
            if (positions) positions->push_back((int)i);
            ++p;
        } else {
            consecutive = 0;
            if (!anyMatched) ++unmatchedLeading;
            else --score;
        }
        prev = c;
    }
    if (p != pattern.size()) return false;
    outScore = score;
    return true;
}

bool TypeMatches(AssetType type, const std::string& value) {
    static const char* names[] = { "texture", "model", "audio" };
    std::string_view name = names[(int)type];
    return value.size() <= name.size() && name.substr(0, value.size()) == value;
}

bool MatchEntry(const ParsedQuery& query, const AssetEntry& e, uint64_t now, int& score) {
    score = 0;
    for (const SearchTerm& term : query.terms) {
        int termScore = 0;
        switch (term.kind) {
        case SearchTermKind::Fuzzy:
            if (FuzzyMatch(term.value, e.nameLower, termScore)) termScore += 40;
            else if (!FuzzyMatch(term.value, e.relPathLower, termScore)) return false;
            score += termScore;
            break;
        case SearchTermKind::Exact:
            if (e.nameLower.find(term.value) == std::string::npos &&
                e.relPathLower.find(term.value) == std::string::npos) return false;
            break;
        case SearchTermKind::Negated:
            if (e.nameLower.find(term.value) != std::string::npos ||
                e.relPathLower.find(term.value) != std::string::npos) return false;
            break;
        case SearchTermKind::Extension: {
            std::string_view ext = e.ext.empty() ? std::string_view{} : std::string_view(e.ext).substr(1);
            if (std::find(term.values.begin(), term.values.end(), ext) == term.values.end()) return false;
            break;
        }
        case SearchTermKind::Type:
            if (std::none_of(term.values.begin(), term.values.end(), [&](const auto& v) { return TypeMatches(e.type, v); })) return false;
            break;
        case SearchTermKind::Pack: {
            std::string pack = ToLowerAscii(e.pack);
            if (!FuzzyMatch(term.value, pack, termScore)) return false;
            score += termScore;
            break;
        }
        case SearchTermKind::Path:
            if (!FuzzyMatch(term.value, e.relPathLower, termScore)) return false;
            score += termScore;
            break;
        case SearchTermKind::Size:
            if ((term.comparison == '>' && e.size <= term.number) ||
                (term.comparison == '<' && e.size >= term.number)) return false;
            break;
        case SearchTermKind::Age: {
            uint64_t age = now > e.mtime ? now - e.mtime : 0;
            if ((term.comparison == '>' && age <= term.number) ||
                (term.comparison == '<' && age >= term.number)) return false;
            break;
        }
        }
    }
    return true;
}

bool CanNarrow(std::string_view oldText, std::string_view newText,
               const ParsedQuery& oldQuery, const ParsedQuery& newQuery) {
    if (oldText.empty() || newText.size() <= oldText.size() || newText.substr(0, oldText.size()) != oldText)
        return false;
    if (oldQuery.terms.size() != newQuery.terms.size() || oldQuery.terms.empty()) return false;
    for (size_t i = 0; i + 1 < oldQuery.terms.size(); ++i)
        if (oldQuery.terms[i].kind != newQuery.terms[i].kind || oldQuery.terms[i].value != newQuery.terms[i].value)
            return false;
    SearchTermKind kind = newQuery.terms.back().kind;
    if (kind != SearchTermKind::Fuzzy && kind != SearchTermKind::Pack && kind != SearchTermKind::Path)
        return false;
    return oldQuery.terms.back().kind == kind &&
           newQuery.terms.back().value.starts_with(oldQuery.terms.back().value);
}

} // namespace

bool SearchNameMatchPositions(const ParsedQuery& query, const std::string& nameLower,
                              std::vector<int>& positions) {
    positions.clear();
    for (const SearchTerm& term : query.terms) {
        if (term.kind == SearchTermKind::Fuzzy) {
            std::vector<int> matched;
            int score = 0;
            if (FuzzyMatch(term.value, nameLower, score, &matched))
                positions.insert(positions.end(), matched.begin(), matched.end());
        } else if (term.kind == SearchTermKind::Exact) {
            size_t start = nameLower.find(term.value);
            if (start != std::string::npos)
                for (size_t i = 0; i < term.value.size(); ++i) positions.push_back((int)(start + i));
        }
    }
    std::sort(positions.begin(), positions.end());
    positions.erase(std::unique(positions.begin(), positions.end()), positions.end());
    return !positions.empty();
}

void App::RebuildFilterIfDirty() {
    if (!filterDirty) return;
    filterDirty = false;

    std::string queryText = searchBuf;
    ParsedQuery query = ParseQuery(queryText);
    ParsedQuery previous = ParseQuery(lastQuery);
    bool hasQuery = !query.terms.empty();
    bool sameSidebarFilters = std::equal(std::begin(typeSel), std::end(typeSel), std::begin(lastTypeSel)) &&
                              selectedPacks == lastSelectedPacks;
    bool narrow = hasQuery && sameSidebarFilters &&
                  CanNarrow(lastQuery, queryText, previous, query);

    filtered.clear();
    bool anyType = typeSel[0] || typeSel[1] || typeSel[2];
    std::vector<std::pair<int, int>> scored;
    std::vector<int> candidates;
    const std::vector<int>* source = narrow ? &lastCandidates : nullptr;
    size_t sourceSize = source ? source->size() : db.entries.size();
    candidates.reserve(sourceSize);
    uint64_t now = (uint64_t)std::chrono::duration_cast<std::chrono::seconds>(
        std::chrono::system_clock::now().time_since_epoch()).count();

    for (size_t n = 0; n < sourceSize; ++n) {
        int i = source ? (*source)[n] : (int)n;
        const AssetEntry& e = db.entries[i];
        if (anyType && !typeSel[(int)e.type]) continue;
        if (!selectedPacks.empty() && !selectedPacks.count(e.pack)) continue;
        int score = 0;
        if (hasQuery && !MatchEntry(query, e, now, score)) continue;
        candidates.push_back(i);
        if (hasQuery) scored.emplace_back(score, i);
        else filtered.push_back(i);
    }

    totalFilterMatches = candidates.size();
    resultTypeCounts[0] = resultTypeCounts[1] = resultTypeCounts[2] = 0;
    resultExtCounts.clear();
    for (int i : candidates) {
        const AssetEntry& e = db.entries[i];
        ++resultTypeCounts[(int)e.type];
        if (e.ext.size() > 1) ++resultExtCounts[e.ext.substr(1)];
    }
    if (hasQuery) {
        constexpr size_t kMaxResults = 5000;
        auto byScore = [](const auto& a, const auto& b) { return a.first > b.first; };
        if (db.entries.size() > 200000 && scored.size() > kMaxResults) {
            std::partial_sort(scored.begin(), scored.begin() + kMaxResults, scored.end(), byScore);
            scored.resize(kMaxResults);
        } else {
            std::stable_sort(scored.begin(), scored.end(), byScore);
        }
        filtered.reserve(scored.size());
        for (const auto& item : scored) filtered.push_back(item.second);
    }

    if (sortMode != 0) {
        std::stable_sort(filtered.begin(), filtered.end(), [&](int a, int b) {
            const AssetEntry& left = db.entries[a];
            const AssetEntry& right = db.entries[b];
            if (sortMode == 2) return left.mtime > right.mtime;
            if (sortMode == 3) return left.size > right.size;
            return left.nameLower < right.nameLower;
        });
    }

    parsedQuery = query;
    lastQuery = std::move(queryText);
    lastCandidates = std::move(candidates);
    std::copy(std::begin(typeSel), std::end(typeSel), std::begin(lastTypeSel));
    lastSelectedPacks = selectedPacks;
}
