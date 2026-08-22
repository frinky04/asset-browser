#pragma once

#include <cstdint>
#include <string>
#include <string_view>
#include <vector>
#include <functional>
#include <set>
#include <unordered_map>
#include "assets.h"

enum class SearchTermKind : uint8_t {
    Fuzzy,
    Exact,
    Negated,
    Extension,
    Type,
    Pack,
    Size,
    Age,
    Path,
    Indexed,   // indexed:yes|no (semantic index membership; resolved by the app)
};

struct SearchTerm {
    SearchTermKind kind = SearchTermKind::Fuzzy;
    std::string value;
    std::vector<std::string> values;
    char comparison = 0;
    uint64_t number = 0;
};

struct ParsedQuery {
    std::vector<SearchTerm> terms;
};

ParsedQuery ParseQueryText(std::string_view input);

// Free-text (fuzzy) terms joined with spaces: the part of a query a semantic search embeds.
std::string SemanticTextFromQuery(const ParsedQuery& query);

bool SearchNameMatchPositions(const ParsedQuery& query, const std::string& nameLower,
                              std::vector<int>& positions);

// True when `newQuery` can only match a subset of `oldQuery`'s results (typing extends a term).
bool CanNarrow(std::string_view oldText, std::string_view newText,
               const ParsedQuery& oldQuery, const ParsedQuery& newQuery);

struct EmbedIndex;

// One filter/rank pass over a library. GUI-free; the app and the CLI both use it.
struct FilterRequest {
    ParsedQuery query;
    const bool* typeSel = nullptr;                   // [3] by AssetType; null/all-false = every type
    const std::set<std::string>* packs = nullptr;    // empty/null = every pack
    int sortMode = 0;                                // 0 relevance, 1 name, 2 newest, 3 largest
    const EmbedIndex* index = nullptr;               // for indexed: and semantic ranking
    std::function<bool(const AssetEntry&)> excluded; // entries kept out of semantic results
    const std::vector<float>* semanticQuery = nullptr; // non-empty = rank by cosine instead of fuzzy
    const std::vector<int>* source = nullptr;        // candidate subset to narrow (null = all entries)
};

struct FilterResult {
    std::vector<int> filtered;        // display order
    std::vector<int> candidates;      // everything that passed the filters, unsorted
    std::vector<float> semanticScore; // per entry, only when semantic
    int typeCounts[3] = {0, 0, 0};
    std::unordered_map<std::string, int> extCounts;
};

FilterResult FilterAssets(const std::vector<AssetEntry>& entries, const FilterRequest& req);
