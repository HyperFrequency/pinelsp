package tree_sitter_pine_test

import (
	"testing"

	tree_sitter "github.com/tree-sitter/go-tree-sitter"
	tree_sitter_pine "github.com/hyperfrequency/pinelsp/bindings/go"
)

func TestCanLoadGrammar(t *testing.T) {
	language := tree_sitter.NewLanguage(tree_sitter_pine.Language())
	if language == nil {
		t.Errorf("Error loading Pine grammar")
	}
}
