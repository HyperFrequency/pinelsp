#include "tree_sitter/alloc.h"
#include "tree_sitter/array.h"
#include "tree_sitter/parser.h"

enum TokenType {
	INDENT,
	DEDENT,
	NEWLINE,
};

typedef struct {
	Array(uint16_t) indents;
} Scanner;

static inline void advance(TSLexer *lexer) {
	lexer->advance(lexer, false);
}

static inline void skip(TSLexer *lexer) {
	lexer->advance(lexer, true);
}

bool tree_sitter_pine_external_scanner_scan(void *payload, TSLexer *lexer, const bool *valid_symbols) {
	Scanner *scanner = (Scanner *)payload;

	lexer->mark_end(lexer);

	bool found_end_of_line = false;
	uint32_t indent_length = 0;

	for (;;) {
		switch (lexer->lookahead) {
			case '\n': {
				found_end_of_line = true;
				indent_length = 0;
				skip(lexer);
				break;
			}
			case ' ': {
				indent_length++;
				skip(lexer);
				break;
			}
			case '\r':
			case '\f': {
				indent_length = 0;
				skip(lexer);
				break;
			}
			case '\t': {
				indent_length += 4;
				skip(lexer);
				break;
			}
			case '\\': {
				skip(lexer);
				if (lexer->lookahead == '\r') {
					skip(lexer);
				}
				if (lexer->lookahead == '\n' || lexer->eof(lexer)) {
					skip(lexer);
				} else {
					return false;
				}
				break;
			}
			default: {
				if (lexer->eof(lexer)) {
					indent_length = 0;
					found_end_of_line = true;
				}
				goto next;
			}
		}
	}

next:
	// A line whose first non-whitespace char is `[` always begins a new
	// statement (tuple destructuring), never a line continuation: a wrapped
	// subscript or array literal keeps its `[` on the same physical line as its
	// base/opening paren (see subscript-vs-tuple.pine:5). Force the statement
	// break here, BEFORE the `% 4` continuation check below, so that an
	// odd-indented tuple line (e.g. indentation-edge-cases.pine:11 ` [c, d] =`)
	// does not get swallowed as a continuation — which would otherwise suppress
	// the NEWLINE that closes the preceding comment and turn it into an ERROR.
	//
	// The valid_symbols guard is load-bearing: it only forces the break where
	// the grammar already expects a statement boundary. The one legitimate
	// `[`-starting continuation in the corpus (function-arg-continuation.pine:65
	// `    [1, 2, 3])` inside an open `func(... =` paren) sits where neither
	// NEWLINE nor DEDENT is valid, so the break does not fire there.
	if (found_end_of_line && !lexer->eof(lexer) && lexer->lookahead == '[' &&
	    (valid_symbols[NEWLINE] || valid_symbols[DEDENT])) {
		goto emit_break;
	}

	if (indent_length % 4 != 0) {
		// line continue
		return false;
	}

	// Leading-operator line continuation: if the next line begins with a
	// continuation operator, suppress the INDENT/DEDENT/NEWLINE break so the
	// expression stays open across the physical line break. In Pine v6 a line
	// can only legitimately START with `?`/`:` (ternary arms — switch arms use
	// `=>`) or `.` (attribute/method chains), so treating a leading one of
	// these as a continuation is unambiguous.
	//
	// `?`/`:` are handled with pure lookahead (no advance). For `.` we need one
	// char past the dot to distinguish an attribute access (`.method`) from a
	// float literal (`.5`); we only treat it as a continuation when a letter or
	// underscore follows. We mark_end before skipping the dot so that, if the
	// dot is NOT followed by a letter/underscore, the lexer's reported end stays
	// at the dot and the consumed character does not corrupt the token stream.
	if (found_end_of_line && !lexer->eof(lexer)) {
		int32_t c = lexer->lookahead;
		if (c == '?' || c == ':') {
			return false;
		}
		if (c == '.') {
			lexer->mark_end(lexer);
			skip(lexer);
			int32_t after_dot = lexer->lookahead;
			if (after_dot == '_' || (after_dot >= 'a' && after_dot <= 'z') ||
			    (after_dot >= 'A' && after_dot <= 'Z')) {
				return false;
			}
			// Not an attribute access (e.g. `.5`): fall through to emit the
			// break. mark_end above keeps the reported token boundary at the
			// dot, so the consumed `.` is not lost.
		}
	}

emit_break:
	if (found_end_of_line) {
		if (scanner->indents.size > 0) {
			uint16_t current_indent_length = *array_back(&scanner->indents);

			if (valid_symbols[INDENT] && indent_length > current_indent_length) {
				array_push(&scanner->indents, indent_length);
				lexer->result_symbol = INDENT;
				return true;
			}

			if (valid_symbols[DEDENT] && indent_length < current_indent_length) {
				array_pop(&scanner->indents);
				lexer->result_symbol = DEDENT;
				return true;
			}
		}

		if (valid_symbols[NEWLINE]) {
			lexer->result_symbol = NEWLINE;
			return true;
		}
	}

	return false;
}


unsigned tree_sitter_pine_external_scanner_serialize(void *payload, char *buffer) {
	Scanner *scanner = (Scanner *)payload;

	size_t size = 0;

	for (uint32_t i = 1; i < scanner->indents.size && size < TREE_SITTER_SERIALIZATION_BUFFER_SIZE; i ++) {
		buffer[size++] = (char)*array_get(&scanner->indents, i);
	}
	return size;
}

void tree_sitter_pine_external_scanner_deserialize(void *payload, const char *buffer, unsigned length) {
	Scanner *scanner = (Scanner *)payload;

	array_delete(&scanner->indents);
	array_push(&scanner->indents, 0);

	if (length <= 0) {
		return;
	}

	for (size_t size = 0; size < length; size ++) {
		array_push(&scanner->indents, (unsigned char)buffer[size]);
	}
}

void *tree_sitter_pine_external_scanner_create() {
	Scanner *scanner = (Scanner *)ts_calloc(1, sizeof(Scanner));
	array_init(&scanner->indents);
	tree_sitter_pine_external_scanner_deserialize(scanner, NULL, 0);
	return scanner;
}

void tree_sitter_pine_external_scanner_destroy(void *payload) {
	Scanner *scanner = (Scanner *)payload;
	array_delete(&scanner->indents);
	free(scanner);
}

