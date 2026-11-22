import Foundation
import Testing
import TaisceKit
@testable import Taisce

/// Search and New doc fixes from the 2026-11-22 review.
@MainActor @Suite(.serialized, .timeLimit(.minutes(1))) struct SearchFixTests {
    static func answer(_ q: String, pending: Bool, body: String = "b") -> AskResult {
        AskResult(question: q, docID: "doc-\(q)", sources: 1, docs: 1, text: AskAnswerText(body: body, sources: [], pending: pending))
    }

    /// #9: while a book search waits out the typing it says so; the last
    /// words' results never stand for the new ones.
    @Test func bookSearchWaitsAsLoading() {
        #expect(SearchBooks.waiting(for: "dune") == .loading)
        #expect(SearchBooks.waiting(for: "  ") == .idle)
        #expect(SearchBooks.waiting(for: "") == .idle)
    }
}
