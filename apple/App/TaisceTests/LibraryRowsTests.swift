import Foundation
import Observation
import SwiftUI
import Testing
import TaisceKit
import UIKit
@testable import Taisce

/// Review 2026-11-22 #8: opening the Library asks for the rows on screen,
/// a time arriving redraws its row only, and formatters are made once.
@MainActor @Suite(.serialized, .timeLimit(.minutes(1))) struct LibraryRowsTests {
    func fires(_ track: () -> Void, _ change: () -> Void) -> Bool {
        let f = Flag()
        withObservationTracking(track) { f.set() }
        change()
        return f.value
    }

    /// A row reads its own entry: another doc's time arriving tells it nothing.
    @Test func aTimeArrivingTellsOnlyItsRow() {
        let times = EditedTimes()
        let a = times.entry("a"), b = times.entry("b")
        let t = Date(timeIntervalSince1970: 1_790_000_000)
        #expect(!fires({ _ = a.date }) { times.follow(["b": EditMeta(author: "x", date: t)]) })
        #expect(b.date == t && a.date == nil)
        #expect(fires({ _ = a.date }) { times.follow(["a": EditMeta(author: "x", date: t), "b": EditMeta(author: "x", date: t)]) })
        // the same times again: nobody is told
        #expect(!fires({ _ = a.date; _ = b.date }) { times.follow(["a": EditMeta(author: "x", date: t), "b": EditMeta(author: "x", date: t)]) })
        // a row made later starts with what is known
        #expect(times.entry("b") === b && times.entry("c").date == nil)
        times.follow([:])
        #expect(a.date == nil && b.date == nil)
    }

    /// The model keeps its rows' entries in step with `editMeta`, and the
    /// Library tree's body never reads the times.
    @Test func theModelsEntriesFollowItsMetadata() {
        let m = AppModel()
        let row = m.editedTimes.entry("d1")
        let t = Date(timeIntervalSince1970: 1_790_000_000)
        #expect(!fires({ _ = m.editedTimes.entry("d1") }) { m.editMeta["d1"] = EditMeta(author: "Ann", date: t) }, "looking a row up depends on nothing")
        #expect(row.date == t)
    }

    /// Opening a long Library asks for the rows on screen, not every row.
    @Test func onlyRowsOnScreenAskForTheirTime() async throws {
        let nodes = (0..<300).map { LibraryNode(doc: DocInfo(id: "r\(String(format: "%03d", $0))", title: "Doc \($0)"), subtitle: nil, children: nil) }
        let asked = Asked()
        let view = NavigationStack {
            LibraryContent(nodes: nodes, loaded: true, onRowAppear: { asked.add($0) })
        }
        let scene = try #require(UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }.first)
        let window = UIWindow(windowScene: scene)
        window.rootViewController = UIHostingController(rootView: view)
        window.makeKeyAndVisible()
        defer { window.isHidden = true }
        for _ in 0..<100 where asked.ids.isEmpty { try await Task.sleep(for: .milliseconds(20)) }
        try await Task.sleep(for: .milliseconds(200))
        let ids = asked.ids
        #expect(!ids.isEmpty, "the rows on screen ask")
        #expect(ids.count < 60, "asked for \(ids.count) of 300 rows")
        #expect(ids.contains("r000"))
        #expect(!ids.contains("r299"))
    }

    /// The Library's and Today's dates come from formatters made once.
    @Test func formattersAreMadeOnce() {
        let cal = Calendar(identifier: .gregorian)
        let now = Date(timeIntervalSince1970: 1_790_000_000)
        _ = ShortWhen.string(now.addingTimeInterval(-60), now: now, calendar: cal)
        _ = RelativeTime.dayLine(now, calendar: cal)
        let before = DateFormats.count
        for i in 0..<200 {
            _ = ShortWhen.string(now.addingTimeInterval(Double(-60 * i)), now: now, calendar: cal)
            _ = RelativeTime.dayLine(now.addingTimeInterval(Double(i * 3600)), calendar: cal)
        }
        #expect(DateFormats.count == before, "a formatter per call")
        // what they say is what a fresh formatter said
        var dublin = Calendar(identifier: .gregorian)
        dublin.timeZone = TimeZone(identifier: "Europe/Dublin")!
        dublin.locale = Locale(identifier: "en_IE")
        let f = DateFormatter()
        f.calendar = dublin
        f.timeZone = dublin.timeZone
        f.locale = dublin.locale
        f.setLocalizedDateFormatFromTemplate("d MMM yyyy")
        let old = Date(timeIntervalSince1970: 1_700_000_000)
        #expect(ShortWhen.string(old, now: now, calendar: dublin) == f.string(from: old))
        // another time zone gets a formatter of its own
        var tokyo = dublin
        tokyo.timeZone = TimeZone(identifier: "Asia/Tokyo")!
        let n = DateFormats.count
        _ = ShortWhen.string(old, now: now, calendar: tokyo)
        #expect(DateFormats.count == n + 1)
    }
}

/// Ids asked for, from any thread.
final class Asked: @unchecked Sendable {
    private let lock = NSLock()
    private var seen: [String] = []
    func add(_ id: String) { lock.withLock { if !seen.contains(id) { seen.append(id) } } }
    var ids: [String] { lock.withLock { seen } }
}
