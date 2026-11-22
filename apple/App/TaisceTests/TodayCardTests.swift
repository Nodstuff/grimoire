import Foundation
import Observation
import Testing
import TaisceKit
@testable import Taisce

/// Review 2026-11-22 #13: Today reads the fleet's card summary, which
/// changes only when what the card says changes, not on every fleet update.
@MainActor @Suite struct TodayCardTests {
    /// Did reading `todayCard` get told of a change while `body` ran?
    func notifies(_ store: FleetStore, _ body: () -> Void) -> Bool {
        let fired = Flag()
        withObservationTracking { _ = store.todayCard } onChange: { fired.set() }
        body()
        return fired.value
    }

    @Test func aFleetUpdateThatLeavesTheCardAsItWasNotifiesNothing() {
        let s = FleetStore()
        let working = FleetTests.session("s1", "taisce/main", .working)
        #expect(notifies(s) { s.take(.change(.fleet(Fleet(rev: 1, runtimes: [FleetTests.runtime()], sessions: [working])), rev: 1)) })
        let card = s.todayCard
        #expect(card?.attention == false)
        // the session's tool and note move on: the card says the same
        var busier = working
        busier.tool = "Bash"
        busier.note = "building"
        #expect(!notifies(s) { s.take(.change(.session(busier), rev: 2)) }, "the card did not change")
        #expect(s.fleet.rev == 2 && s.todayCard == card)
        // it needs its person now: the card changes, and says so
        var waiting = busier
        waiting.state = .needsYou
        #expect(notifies(s) { s.take(.change(.session(waiting), rev: 3)) })
        #expect(s.todayCard?.attention == true)
        // signed out: no card
        #expect(notifies(s) { s.reset() })
        #expect(s.todayCard == nil)
    }

    @Test func theCardIsTheOneTodayShowedBefore() {
        let s = FleetStore()
        s.take(.change(.fleet(FleetTests.fleet), rev: 5))
        #expect(s.todayCard == FleetCard(FleetTests.fleet, available: true))
    }
}

/// A flag set from any thread.
final class Flag: @unchecked Sendable {
    private let lock = NSLock()
    private var on = false
    func set() { lock.withLock { on = true } }
    var value: Bool { lock.withLock { on } }
}
