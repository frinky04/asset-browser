#include "search.h"
#include "embed.h"
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
            } else if (field == "indexed" && !value.empty()) {
                term.kind = SearchTermKind::Indexed;
                term.number = (value == "yes" || value == "true" || value == "1") ? 1 : 0;
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

// skipFuzzy: free-text terms are handled elsewhere (semantic ranking); only apply filters.
bool MatchEntry(const ParsedQuery& query, const AssetEntry& e, uint64_t now, int& score, bool skipFuzzy = false) {
    score = 0;
    for (const SearchTerm& term : query.terms) {
        int termScore = 0;
        switch (term.kind) {
        case SearchTermKind::Fuzzy:
            if (skipFuzzy) break;
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
        case SearchTermKind::Indexed:
            break; // needs the embedding index; applied by the caller
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

} // namespace

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



ParsedQuery ParseQueryText(std::string_view input) { return ParseQuery(input); }

std::string SemanticTextFromQuery(const ParsedQuery& query) {
    std::string text;
    for (const SearchTerm& term : query.terms) {
        if (term.kind != SearchTermKind::Fuzzy) continue;
        if (!text.empty()) text += ' ';
        text += term.value;
    }
    return text;
}

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

FilterResult FilterAssets(const std::vector<AssetEntry>& entries, const FilterRequest& req) {
    FilterResult out;
    const ParsedQuery& query = req.query;
    bool hasQuery = !query.terms.empty();
    bool semantic = req.semanticQuery && !req.semanticQuery->empty() && req.index;
    int indexedFilter = 0; // 0 none, 1 only indexed, -1 only un-indexed
    for (const SearchTerm& t : query.terms)
        if (t.kind == SearchTermKind::Indexed) indexedFilter = t.number ? 1 : -1;

    bool anyType = req.typeSel && (req.typeSel[0] || req.typeSel[1] || req.typeSel[2]);
    std::vector<std::pair<int, int>> scored;
    std::vector<std::pair<float, int>> semanticScored;
    if (semantic) out.semanticScore.assign(entries.size(), 0.0f);
    size_t sourceSize = req.source ? req.source->size() : entries.size();
    out.candidates.reserve(sourceSize);
    uint64_t now = (uint64_t)std::chrono::duration_cast<std::chrono::seconds>(
        std::chrono::system_clock::now().time_since_epoch()).count();
    auto excluded = [&](const AssetEntry& e) { return req.excluded && req.excluded(e); };

    for (size_t n = 0; n < sourceSize; ++n) {
        int i = req.source ? (*req.source)[n] : (int)n;
        const AssetEntry& e = entries[i];
        if (anyType && !req.typeSel[(int)e.type]) continue;
        if (req.packs && !req.packs->empty() && !req.packs->count(e.pack)) continue;
        if (indexedFilter) {
            bool isIndexed = req.index && !excluded(e) && req.index->VectorFor(e) != nullptr;
            if ((indexedFilter > 0) != isIndexed) continue;
        }
        int score = 0;
        if (semantic) {
            // Rank every indexed asset by cosine to the query; text terms are
            // the query itself, so only the filter terms narrow the set.
            const float* v = excluded(e) ? nullptr : req.index->VectorFor(e);
            if (!v) continue;
            if (hasQuery && !MatchEntry(query, e, now, score, /*skipFuzzy*/ true)) continue;
            float dot = 0;
            for (int k = 0; k < req.index->dims; ++k) dot += v[k] * (*req.semanticQuery)[k];
            out.semanticScore[i] = dot;
            out.candidates.push_back(i);
            semanticScored.emplace_back(dot, i);
            continue;
        }
        if (hasQuery && !MatchEntry(query, e, now, score)) continue;
        out.candidates.push_back(i);
        if (hasQuery) scored.emplace_back(score, i);
        else out.filtered.push_back(i);
    }

    for (int i : out.candidates) {
        const AssetEntry& e = entries[i];
        ++out.typeCounts[(int)e.type];
        if (e.ext.size() > 1) ++out.extCounts[e.ext.substr(1)];
    }
    if (semantic) {
        std::stable_sort(semanticScored.begin(), semanticScored.end(),
                         [](const auto& a, const auto& b) { return a.first > b.first; });
        out.filtered.reserve(semanticScored.size());
        for (const auto& item : semanticScored) out.filtered.push_back(item.second);
    } else if (hasQuery) {
        constexpr size_t kMaxResults = 5000;
        auto byScore = [](const auto& a, const auto& b) { return a.first > b.first; };
        if (entries.size() > 200000 && scored.size() > kMaxResults) {
            std::partial_sort(scored.begin(), scored.begin() + kMaxResults, scored.end(), byScore);
            scored.resize(kMaxResults);
        } else {
            std::stable_sort(scored.begin(), scored.end(), byScore);
        }
        out.filtered.reserve(scored.size());
        for (const auto& item : scored) out.filtered.push_back(item.second);
    }

    if (req.sortMode != 0) {
        std::stable_sort(out.filtered.begin(), out.filtered.end(), [&](int a, int b) {
            const AssetEntry& left = entries[a];
            const AssetEntry& right = entries[b];
            if (req.sortMode == 2) return left.mtime > right.mtime;
            if (req.sortMode == 3) return left.size > right.size;
            return left.nameLower < right.nameLower;
        });
    }
    return out;
}
