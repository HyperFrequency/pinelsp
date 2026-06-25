import XCTest
import SwiftTreeSitter
import TreeSitterPine

final class TreeSitterPineTests: XCTestCase {
    func testCanLoadGrammar() throws {
        let parser = Parser()
        let language = Language(language: tree_sitter_pine())
        XCTAssertNoThrow(try parser.setLanguage(language),
                         "Error loading Pine grammar")
    }
}
