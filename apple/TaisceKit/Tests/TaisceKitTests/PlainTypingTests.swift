import Testing
@testable import TaisceKit

@Suite struct PlainTypingTests {
    @Test func typedSmartQuotesAndDashesComeBackStraight() {
        #expect(PlainTyping.straighten(old: "f(", new: "f(\u{201C}") == "f(\"")
        #expect(PlainTyping.straighten(old: "f(\"abc", new: "f(\"abc\u{201D}") == "f(\"abc\"")
        #expect(PlainTyping.straighten(old: "x", new: "x\u{2019}") == "x'")
        #expect(PlainTyping.straighten(old: "a -", new: "a \u{2014}") == "a --")
        // a paste is straightened too
        #expect(PlainTyping.straighten(old: "", new: "longestNoRepeat(\u{201C}abcab\u{201D})") == "longestNoRepeat(\"abcab\")")
    }

    @Test func textAlreadyThereIsLeftAlone() {
        let old = "s := \"\u{201C}quoted\u{201D}\"\n"
        #expect(PlainTyping.straighten(old: old, new: old + "x") == old + "x")
        #expect(PlainTyping.straighten(old: old, new: String(old.dropLast())) == String(old.dropLast()))
    }
}
