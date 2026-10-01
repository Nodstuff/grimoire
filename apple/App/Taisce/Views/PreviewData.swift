#if DEBUG
import Foundation
import TaisceKit

/// Fixtures for #Preview blocks and tests only; the app never shows them.
enum PreviewData {
    static let now = Date(timeIntervalSince1970: 1_790_000_000)

    static func ago(_ seconds: TimeInterval) -> Date { now.addingTimeInterval(-seconds) }

    static let docs: [DocInfo] = [
        DocInfo(id: "g", title: "Grimoire", sortKey: "a"),
        DocInfo(id: "g-ios", parentID: "g", title: "iOS app", sortKey: "a"),
        DocInfo(id: "g-ios-design", parentID: "g-ios", title: "Design spec", sortKey: "a"),
        DocInfo(id: "g-ios-sync", parentID: "g-ios", title: "Sync contract", sortKey: "b"),
        DocInfo(id: "g-road", parentID: "g", title: "Roadmap", sortKey: "b"),
        DocInfo(id: "g-arch", parentID: "g", title: "Architecture", sortKey: "c"),
        DocInfo(id: "home", title: "Home", sortKey: "b"),
        DocInfo(id: "home-garden", parentID: "home", title: "Garden plan", sortKey: "a"),
        DocInfo(id: "todo", title: "To-do", sortKey: "c"),
    ]

    static let index = DocIndex(docs)

    static var today: String { Due.today(now: now).dateString }

    static func day(_ offset: Int) -> Due {
        let d = Calendar.current.date(byAdding: .day, value: offset, to: now) ?? now
        return Due.today(now: d)
    }

    static func timed(_ offset: Int, _ hour: Int, _ minute: Int = 0) -> Due {
        var d = day(offset)
        d.hour = hour
        d.minute = minute
        return d
    }

    static let entries: [TodoEntry] = [
        TodoEntry(date: day(-2).dateString, itemID: "0-a1", text: "Send the **hub** deploy notes", due: day(-1), overdue: true),
        TodoEntry(date: today, itemID: "1-b2", text: "Review the iOS design pass", due: timed(0, 15), note: "Screens for Today and Library first"),
        TodoEntry(date: today, itemID: "2-c3", text: "Renew the TLS cert on taisce.null.ie", due: day(0)),
        TodoEntry(date: today, itemID: "3-d4", text: "Water the tomatoes"),
        TodoEntry(date: today, itemID: "4-e5", text: "Call Ann about the garden", due: timed(2, 10, 30)),
        TodoEntry(date: today, itemID: "5-f6", text: "Ship 0.9 with the Taisce rename", due: day(9)),
    ]

    static var board: TodoBoard {
        TodoBoard.build(dated: entries.filter { $0.due != nil }, undated: entries.filter { $0.due == nil }, now: now)
    }

    static let cards: [DocCardModel] = [
        DocCardModel(id: "g-ios-design", title: "Design spec", folder: "iOS app", edited: ago(2 * 3600)),
        DocCardModel(id: "g-road", title: "Roadmap", folder: "Grimoire", edited: ago(26 * 3600)),
        DocCardModel(id: "home-garden", title: "Garden plan", folder: "Home", edited: ago(4 * 86400)),
        DocCardModel(id: "g-arch", title: "Architecture", folder: "Grimoire", edited: nil),
    ]

    static let markdown: [String] = [
        "---\ntags:\n  - design\n  - ios\n---",
        "Taisce is the reading surface for the library: **fast**, *calm*, and offline-first. See [[Sync contract]] for how the cache stays fresh, and `SyncEngine` for the code.",
        "## Principles",
        "- Dark-first, with a light variant\n- Serif titles, SF Pro body\n- Touch targets of at least 44pt",
        "## Open items",
        "- [x] Tokens and type scale\n- [ ] Library tree with disclosure\n- [ ] Doc reading view",
        "> [!NOTE]\n> Editing is the next pass; this one reads.",
        "> Simplicity is prerequisite for reliability.",
        "| Screen | Owner | Status |\n|---|---|:-:|\n| Today | Tom | done |\n| Library | claude | in review |\n| Search | claude | todo |",
        "```swift\nlet engine = SyncEngine(api: api, cache: cache)\nawait engine.start() // foreground only; APNs later\n```",
        "```mermaid\ngraph LR; App-->Cache; Cache-->Server\n```",
        "1. Seed a scratch daemon\n2. Point the app at it\n3. Screenshot every screen",
    ]

    static var page: DocPage {
        let blocks = markdown.enumerated().map { i, md in
            Block(id: "b\(i)", docID: "g-ios-design", parentID: nil, orderKey: String(i), blockType: md.hasPrefix("---") ? .code : .paragraph, content: md)
        }
        return DocPage.build(title: "Design spec", blocks: blocks)
    }

    static let search = SearchState.build(
        hits: [
            ("s1", "g-ios-design", "Design spec", "The **sync** chip turns green when the outbox is empty and the stream is live."),
            ("s2", "g-ios-sync", "Sync contract", "The client follows a global change cursor: page `/api/changes` from `last_seq`, then follow the sync stream."),
            ("s3", "g-arch", "Architecture", "Federation sync pulls shared docs between daemons through the hub."),
        ],
        query: "sync", index: index,
        tags: ["g-ios-design": ["design", "ios"], "g-ios-sync": ["ios", "sync"], "g-arch": ["architecture"]]
    )
}
#endif
