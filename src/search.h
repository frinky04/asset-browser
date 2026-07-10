#pragma once

#include <cstdint>
#include <string>
#include <vector>

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

bool SearchNameMatchPositions(const ParsedQuery& query, const std::string& nameLower,
                              std::vector<int>& positions);
