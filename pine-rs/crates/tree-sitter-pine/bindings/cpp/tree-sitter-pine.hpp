// tree-sitter-pine — C++ binding (header-only).
//
// The grammar is plain C; C++ consumers compile `src/parser.c` + `src/scanner.c`
// and link against the tree-sitter runtime (libtree-sitter). This header exposes
// the language function and, when the tree-sitter runtime header is available,
// a small RAII convenience wrapper.
#ifndef TREE_SITTER_PINE_HPP
#define TREE_SITTER_PINE_HPP

struct TSLanguage;

extern "C" {
// Returns the tree-sitter Language for Pine Script.
const TSLanguage *tree_sitter_pine();
}

// RAII wrapper is only compiled when the tree-sitter runtime API is present.
#if defined(__has_include)
#if __has_include(<tree_sitter/api.h>)
#include <tree_sitter/api.h>
#include <string_view>

namespace tree_sitter_pine {

/// Owns a TSTree.
class Tree {
public:
    explicit Tree(TSTree *tree) : tree_(tree) {}
    ~Tree() { if (tree_) ts_tree_delete(tree_); }
    Tree(Tree &&o) noexcept : tree_(o.tree_) { o.tree_ = nullptr; }
    Tree &operator=(Tree &&o) noexcept {
        if (this != &o) { if (tree_) ts_tree_delete(tree_); tree_ = o.tree_; o.tree_ = nullptr; }
        return *this;
    }
    Tree(const Tree &) = delete;
    Tree &operator=(const Tree &) = delete;
    TSNode root() const { return ts_tree_root_node(tree_); }
    TSTree *get() const { return tree_; }
private:
    TSTree *tree_;
};

/// Owns a TSParser configured for Pine.
class Parser {
public:
    Parser() : parser_(ts_parser_new()) { ts_parser_set_language(parser_, ::tree_sitter_pine()); }
    ~Parser() { ts_parser_delete(parser_); }
    Parser(const Parser &) = delete;
    Parser &operator=(const Parser &) = delete;
    Tree parse(std::string_view src) {
        return Tree(ts_parser_parse_string(parser_, nullptr, src.data(),
                                           static_cast<uint32_t>(src.size())));
    }
private:
    TSParser *parser_;
};

} // namespace tree_sitter_pine
#endif
#endif

#endif // TREE_SITTER_PINE_HPP
