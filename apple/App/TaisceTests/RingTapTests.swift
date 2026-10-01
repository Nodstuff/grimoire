import Foundation
import Testing
import TaisceKit
@testable import Taisce

@MainActor @Suite(.timeLimit(.minutes(1))) struct RingTapTests {
    let entry = TodoEntry(date: "2026-10-01", itemID: "0-abc", text: "Ship it")

    /// The commit swipe-Done uses: the typed toggle with the workspace clock.
    func commit(into cache: Cache) -> @MainActor (TodoEntry) async -> Void {
        { e in
            _ = try? await cache.enqueueTodoToggle(date: e.date, itemID: e.itemID, done: true, clock: TodoClock().in(.id("w1")))
        }
    }

    @Test func tapChecksAtOnceThenQueuesTheToggle() async throws {
        let cache = try Cache.inMemory()
        let taps = RingTaps(sleep: { _ in })
        taps.tap(entry, commit: commit(into: cache))
        #expect(taps.isChecked(entry), "optimistic: checked before anything is sent")
        await taps.settle()
        let queued = try await cache.pendingOutbox()
        #expect(queued.map(\.path) == ["/api/todo/toggle"])
        let body = try #require(queued.first?.body)
        let obj = try #require(JSONSerialization.jsonObject(with: body) as? [String: Any])
        #expect(obj["item_id"] as? String == "0-abc" && obj["done"] as? Bool == true && obj["workspace"] as? String == "w1")
        #expect(!taps.isChecked(entry), "committed: the board's settled set hides it now")
    }

    @Test func tapTwiceInsideTheFadeIsANoOp() async throws {
        let cache = try Cache.inMemory()
        let taps = RingTaps(sleep: { _ in })
        taps.tap(entry, commit: commit(into: cache))
        taps.tap(entry, commit: commit(into: cache))
        #expect(!taps.isChecked(entry))
        await taps.settle()
        #expect(try await cache.pendingOutbox().isEmpty, "undone: nothing queued")
    }
}
