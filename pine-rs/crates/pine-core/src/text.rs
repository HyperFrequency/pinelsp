//! UTF-16-aware mapping between byte offsets and LSP positions.
//!
//! LSP `Position` uses 0-based line numbers and 0-based **UTF-16** character
//! offsets within the line; tree-sitter and Rust string slicing use **bytes**.
//! `LineIndex` bridges the two. Keep this the single place that does the
//! conversion so multibyte input (emoji/unicode in strings and comments — which
//! the Pine grammar allows) can't silently produce off-by-N columns.

/// Precomputed line-start byte offsets for a source string.
pub struct LineIndex {
    /// Byte offset of the start of each line (index 0 is always 0).
    line_starts: Vec<usize>,
    len: usize,
}

impl LineIndex {
    pub fn new(text: &str) -> Self {
        let mut line_starts = vec![0];
        for (i, b) in text.bytes().enumerate() {
            if b == b'\n' {
                line_starts.push(i + 1);
            }
        }
        Self {
            line_starts,
            len: text.len(),
        }
    }

    /// LSP position (0-based line, 0-based UTF-16 character) -> byte offset into
    /// `text`. Out-of-range lines clamp to end-of-text; an out-of-range column
    /// clamps to end-of-line.
    pub fn offset_at(&self, text: &str, line: u32, character_utf16: u32) -> usize {
        let line = line as usize;
        if line >= self.line_starts.len() {
            return self.len;
        }
        let line_start = self.line_starts[line];
        let line_end = self
            .line_starts
            .get(line + 1)
            .copied()
            .unwrap_or(self.len);
        let mut utf16 = 0u32;
        for (byte_off, ch) in text[line_start..line_end].char_indices() {
            if utf16 >= character_utf16 {
                return line_start + byte_off;
            }
            utf16 += ch.len_utf16() as u32;
        }
        line_end
    }

    /// Byte offset -> LSP position (0-based line, 0-based UTF-16 character).
    pub fn position_at(&self, text: &str, offset: usize) -> (u32, u32) {
        let offset = offset.min(self.len);
        let line = match self.line_starts.binary_search(&offset) {
            Ok(line) => line,
            Err(next) => next - 1,
        };
        let line_start = self.line_starts[line];
        let mut utf16 = 0u32;
        for (byte_off, ch) in text[line_start..].char_indices() {
            if line_start + byte_off >= offset {
                break;
            }
            utf16 += ch.len_utf16() as u32;
        }
        (line as u32, utf16)
    }

    /// Number of lines (>= 1).
    pub fn line_count(&self) -> usize {
        self.line_starts.len()
    }

    /// Byte offset -> tree-sitter point `(row, byte_column)`. NOTE: tree-sitter
    /// columns are **byte** offsets within the line, not UTF-16 — this is the
    /// coordinate space for `InputEdit`, distinct from LSP positions.
    pub fn byte_to_point(&self, byte: usize) -> (usize, usize) {
        let byte = byte.min(self.len);
        let row = match self.line_starts.binary_search(&byte) {
            Ok(r) => r,
            Err(next) => next - 1,
        };
        (row, byte - self.line_starts[row])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_roundtrip() {
        let text = "//@version=6\nplot(close)\n";
        let idx = LineIndex::new(text);
        // start of line 1 ("plot...") is byte 13
        assert_eq!(idx.offset_at(text, 1, 0), 13);
        assert_eq!(idx.position_at(text, 13), (1, 0));
        // "close" starts at column 5 on line 1
        assert_eq!(idx.offset_at(text, 1, 5), 18);
        assert_eq!(idx.position_at(text, 18), (1, 5));
    }

    #[test]
    fn utf16_counts_astral_as_two_units() {
        // 😀 is one Unicode scalar (4 UTF-8 bytes) but TWO UTF-16 code units.
        let text = "x = \"😀ab\"\n";
        let idx = LineIndex::new(text);
        // byte offset of 'a' (after the 4-byte emoji at byte 5): 5 + 4 = 9
        let a_byte = text.find('a').unwrap();
        assert_eq!(a_byte, 9);
        let (line, col) = idx.position_at(text, a_byte);
        assert_eq!(line, 0);
        // columns: x(0) space(1) =(2) space(3) "(4) 😀(5,6) a(7)
        assert_eq!(col, 7, "emoji must count as 2 UTF-16 units");
        // round-trip back to the same byte
        assert_eq!(idx.offset_at(text, line, col), a_byte);
    }

    #[test]
    fn out_of_range_clamps() {
        let text = "abc\n";
        let idx = LineIndex::new(text);
        assert_eq!(idx.offset_at(text, 99, 0), text.len());
        assert_eq!(idx.position_at(text, 9999), idx.position_at(text, text.len()));
    }
}
