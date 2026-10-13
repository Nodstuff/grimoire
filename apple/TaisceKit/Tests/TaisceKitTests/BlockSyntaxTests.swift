import Foundation
import Testing
@testable import TaisceKit

/// A paragraph that begins with block syntax ("- ", "1. ", "# ", "> ",
/// "```") is written escaped, so the server keeps it a paragraph: no change
/// of kind on the echo.
@Suite struct BlockSyntaxTests {
    @Test func paragraphsStartingWithBlockSyntaxStayParagraphs() {
        for text in ["- not a list", "* nor this", "+ nor this", "1. not numbered", "3) nor this", "# not a heading", "###### nor this",
                     "> not a quote", "```not code", "~~~ nor this", "line one\n- not a list either", "---", "| a | b |"] {
            let p = EditorBlockContent.paragraph(AttributedString(text))
            let md = p.markdown
            #expect(EditorBlockContent.parse(markdown: md) == p, "\(text.debugDescription) → \(md.debugDescription)")
            #expect(DocDiff.inferBlockType(md) == .paragraph, "\(md.debugDescription)")
        }
    }
}
