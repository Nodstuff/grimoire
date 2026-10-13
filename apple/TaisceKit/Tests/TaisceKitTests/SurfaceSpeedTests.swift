import Foundation
import Testing
@testable import TaisceKit

/// A big doc stays quick to type in: the per-keystroke work is linear but cheap.
@Suite struct SurfaceSpeedTests {
    static func bigDoc() -> NSMutableAttributedString {
        var blocks: [(id: BlockID, content: EditorBlockContent)] = []
        for i in 0..<1500 {
            switch i % 5 {
            case 0: blocks.append(("h\(i)", .heading(level: 2, AttributedString("Section \(i)"))))
            case 1: blocks.append(("l\(i)", .list([EditorListItem(.bullet, AttributedString("one \(i)")), EditorListItem(.task, AttributedString("two"))])))
            case 2: blocks.append(("c\(i)", .raw("```\ncode \(i)\n```")))
            default: blocks.append(("p\(i)", .paragraph(AttributedString("A paragraph of ordinary text with bold words, number \(i)."))))
            }
        }
        return Surface.document(blocks)
    }

    @Test func aKeystrokeInABigDocIsQuick() {
        let s = Self.bigDoc()
        let clock = ContinuousClock()
        var sel = NSRange(location: s.length / 2, length: 0)
        let took = clock.measure {
            for _ in 0..<10 {
                s.replaceCharacters(in: sel, with: NSAttributedString(string: "x", attributes: Surface.typingAttributes(s, at: sel.location)))
                sel.location += 1
                Surface.normalize(s, selection: &sel, around: NSRange(location: sel.location - 1, length: 1))
            }
        }
        print("10 keystrokes in a \(s.length)-character doc: \(took)")
        #expect(took < .seconds(2))
    }
}
