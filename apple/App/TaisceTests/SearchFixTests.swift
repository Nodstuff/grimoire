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

    /// #11: a second question cancels the first run; the first's late
    /// answer never replaces the second's, and its poll stops.
    @Test func aRepeatAskCancelsTheRunBefore() async throws {
        let r = AskRunner()
        r.every = .milliseconds(10)
        var firstReloads = 0
        var releaseFirst = false
        r.start("first", ask: { q in
            // a request that answers whatever happens to its task (like a URL load that already left)
            while !releaseFirst {
                await withCheckedContinuation { c in DispatchQueue.main.asyncAfter(deadline: .now() + 0.005) { c.resume() } }
            }
            return .answer(Self.answer(q, pending: true))
        }, reload: { a in firstReloads += 1; return a })
        r.start("second", ask: { q in .answer(Self.answer(q, pending: false)) }, reload: { $0 })
        for _ in 0..<200 where r.state == .asking("second") { try await Task.sleep(for: .milliseconds(5)) }
        #expect(r.state == .answer(Self.answer("second", pending: false)))
        releaseFirst = true
        try await Task.sleep(for: .milliseconds(100))
        #expect(r.state == .answer(Self.answer("second", pending: false)), "the first run's late answer landed over the second")
        #expect(firstReloads == 0, "the first run kept polling")
    }

    /// #11: leaving the screen stops following a pending answer.
    @Test func cancelStopsThePoll() async throws {
        let r = AskRunner()
        r.every = .milliseconds(10)
        var reloads = 0
        r.queryChanged("q")
        r.start("q", ask: { q in .answer(Self.answer(q, pending: true)) }, reload: { a in reloads += 1; return a })
        for _ in 0..<200 where reloads < 2 { try await Task.sleep(for: .milliseconds(5)) }
        r.cancel()
        let seen = reloads
        try await Task.sleep(for: .milliseconds(100))
        #expect(seen >= 2 && reloads <= seen + 1, "polled on after leaving: \(seen) → \(reloads)")
        // the synthesis landing ends the poll on its own
        var more = 0
        r.start("q", ask: { q in .answer(Self.answer(q, pending: true)) }, reload: { a in more += 1; return Self.answer(a.question, pending: false, body: "done") })
        for _ in 0..<200 where more == 0 { try await Task.sleep(for: .milliseconds(5)) }
        try await Task.sleep(for: .milliseconds(60))
        #expect(more == 1 && r.state == .answer(Self.answer("q", pending: false, body: "done")))
        // new words forget an answer to other words
        r.queryChanged("other")
        #expect(r.state == .idle)
    }
}
