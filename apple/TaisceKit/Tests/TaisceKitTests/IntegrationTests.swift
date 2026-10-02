import Foundation
import Testing
@testable import TaisceKit

// Integration tests against REAL scratch daemons, started by
// apple/scripts/integration.sh (never ~/.grimoire, never port 7425):
//   TAISCE_IT_URL            a LOCAL-mode daemon (no auth)
//   TAISCE_IT_AUTH_URL       a SERVER-mode daemon (--public-url)
//   TAISCE_IT_ENROLL_URL     a one-time enrollment link for it
//   TAISCE_IT_SOFTPASSKEY    the softpasskey example binary (plays the browser)

enum IT {
    static let env = ProcessInfo.processInfo.environment
    static var local: URL? { env["TAISCE_IT_URL"].flatMap(URL.init(string:)) }
    static var server: URL? { env["TAISCE_IT_AUTH_URL"].flatMap(URL.init(string:)) }

    /// Poll until `check` returns a value (the daemon is asynchronous: the
    /// change log and search index catch up a moment after a write).
    static func eventually<T: Sendable>(
        timeout: Duration = .seconds(10), _ what: String, _ check: @Sendable () async throws -> T?
    ) async throws -> T {
        let deadline = ContinuousClock.now + timeout
        while ContinuousClock.now < deadline {
            if let v = try await check() { return v }
            try await Task.sleep(for: .milliseconds(100))
        }
        throw ITError.timeout(what)
    }
}

enum ITError: Error { case timeout(String), noRedirect(String) }

/// A rich doc: one of every block type the app renders.
let seedMarkdown = """
# Seeded

Intro paragraph with a [[Wiki Target]] link and **bold** text.

## Lists

- one
- two
  - nested
- [ ] a task
- [x] a done task

1. first
2. second

## Table

| a | b |
|---|:-:|
| 1 | 2 |

## Code

```swift
let x = 1
```

```mermaid
graph TD; A-->B
```

> [!NOTE]
> a callout

---
"""

@Suite(.serialized, .enabled(if: IT.local != nil))
struct LocalIntegrationTests {
    let api: APIClient

    init() throws {
        api = APIClient(config: ServerConfig(baseURL: try #require(IT.local)))
    }

    /// A fresh doc filled with `markdown`; returns its tree.
    func seed(_ title: String, _ markdown: String = seedMarkdown) async throws -> DocTree {
        let doc = try await api.createDoc(title: "\(title) \(UUID().uuidString.prefix(8))")
        let out = try await api.proposeMarkdown(ProposeMarkdownRequest(docID: doc.id, baseEpoch: doc.currentEpoch, markdown: markdown))
        #expect(out.verdicts.allSatisfy { $0.applied }, "seed applied green: \(out.verdicts)")
        return try await api.doc(doc.id)
    }

    @Test func treeBootstrapsFromTheSeqHeader() async throws {
        let seeded = try await seed("Bootstrap")
        let (docs, seq) = try await api.treeWithSeq()
        let head = try #require(seq, "X-Grimoire-Seq present")
        #expect(docs.contains { $0.id == seeded.doc.id })
        #expect(try await api.changesHead() >= head)
        // a fresh SyncEngine lands the whole tree and starts at the head
        let cache = try Cache.inMemory()
        try await SyncEngine(api: api, cache: cache).catchUp()
        #expect(try await cache.docs().contains { $0.id == seeded.doc.id })
        #expect(try await cache.lastSeq() >= head)
    }

    @Test func fetchesAndRendersEveryBlockType() async throws {
        let tree = try await seed("Render")
        let types = Set(tree.flattened().map(\.block.blockType))
        #expect(types.isSuperset(of: [.heading, .paragraph, .code, .diagramMermaid]), "types: \(types)")
        let cache = try Cache.inMemory()
        try await cache.storeDoc(tree)
        let nodes = BlockRenderer.render(try await cache.blocks(of: tree.doc.id)).flatMap(\.nodes)
        func has(_ p: (RenderNode) -> Bool) -> Bool { nodes.contains(where: p) }
        #expect(has { if case .heading(1, "Seeded") = $0 { true } else { false } })
        #expect(has { if case let .paragraph(inline) = $0 { inline.contains("[[Wiki Target]]") } else { false } })
        #expect(has { if case let .list(false, _, items) = $0 { items.contains { $0.checked == true } && items.contains { $0.checked == false } } else { false } })
        #expect(has { if case .list(true, 1, _) = $0 { true } else { false } })
        #expect(has { if case let .table(t) = $0 { t.header == ["a", "b"] && t.rows == [["1", "2"]] } else { false } })
        #expect(has { if case .code("swift", _) = $0 { true } else { false } })
        #expect(has { if case .diagram("mermaid", _) = $0 { true } else { false } })
        #expect(has { if case .quote("NOTE", _) = $0 { true } else { false } } || has { if case .quote = $0 { true } else { false } })
        // the markdown export round-trips the content
        #expect(try await api.docMarkdown(tree.doc.id).contains("graph TD"))
    }

    @Test func searchFindsSeededText() async throws {
        let word = "zebra\(UUID().uuidString.prefix(6).lowercased())"
        let tree = try await seed("Search", "# Search\n\nthe \(word) grazes")
        let hits = try await IT.eventually("search hit") {
            let h = try await api.search(word)
            return h.isEmpty ? nil : h
        }
        #expect(hits.contains { $0.block.docID == tree.doc.id })
    }

    @Test func changesPageUntilMoreIsFalse() async throws {
        let head = try await api.changesHead()
        var ids: [DocID] = []
        for i in 0..<5 { ids.append(try await seed("Page \(i)", "# Page \(i)\n\nbody").doc.id) }
        var since = head
        var seen: Set<DocID> = []
        var pages = 0
        while true {
            let page = try await api.changes(since: since, limit: 2)
            pages += 1
            #expect(page.changes.count <= 2)
            #expect(page.changes.allSatisfy { $0.seq > since })
            seen.formUnion(page.changes.map(\.docID))
            since = page.changes.last?.seq ?? page.seq
            if !page.more { break }
        }
        #expect(pages >= 3)
        #expect(Set(ids).isSubset(of: seen))
    }

    @Test func streamDeliversALiveEdit() async throws {
        let tree = try await seed("Stream", "# Stream\n\nbefore")
        let head = try await api.changesHead()
        let para = try #require(tree.flattened().first { $0.block.blockType == .paragraph }?.block)
        let api = self.api
        let received = try await withThrowingTaskGroup(of: Change?.self) { group in
            group.addTask {
                for try await out in api.changeStream(lastEventID: head) {
                    guard case let .event(e) = out, e.event == "change",
                          let c = try? JSONDecoder().decode(Change.self, from: Data(e.data.utf8)),
                          c.docID == tree.doc.id
                    else { continue }
                    return c
                }
                return nil
            }
            group.addTask {
                // edit after the stream is open
                try await Task.sleep(for: .milliseconds(500))
                _ = try await api.propose(ProposeRequest(docID: tree.doc.id, baseEpoch: tree.doc.currentEpoch, ops: [.replace(target: para.id, content: "after")]))
                try await Task.sleep(for: .seconds(10))
                throw ITError.timeout("no change event in 10 s")
            }
            let first = try await group.next() ?? nil
            group.cancelAll()
            return first
        }
        let c = try #require(received)
        #expect(c.kind == .doc && (c.epoch ?? 0) > tree.doc.currentEpoch)
    }

    /// The device's clock rides along; deadlines go out as an all-day date
    /// or a UTC instant. Against a pre-UTC daemon a timed deadline lands as
    /// its local day (the `deadline` that rides along with `due_at`).
    @Test func todoDueAndDeadlineWrite() async throws {
        let clock = TodoClock()
        let today = clock.today
        let tag = "rent\(UUID().uuidString.prefix(6).lowercased())"
        let day = try await api.todoAdd(date: today, text: "pay \(tag)", clock: clock)
        let item = try #require(day.items.first { $0.text.contains(tag) }, "added: \(day.items)")
        func listed() async throws -> TodoDueList.Item? { try await api.todoDue().items.first { $0.itemID == item.id } }

        let todayDue = try #require(Due(today))
        let timed = try #require(Deadline.local(Due(year: todayDue.year, month: todayDue.month, day: todayDue.day, hour: 23, minute: 45)))
        _ = try await api.todoSetDeadline(date: today, itemID: item.id, deadline: timed, clock: clock)
        let t = try #require(try await listed())
        if t.dueAt != nil {
            #expect(t.deadlineValue == timed, "UTC daemon: the instant round-trips")
            #expect(t.dueTime == "23:45" && t.deadline == today, "shown back in the device's zone")
        } else {
            #expect(t.deadline == today, "pre-UTC daemon: filed on the local day")
        }
        #expect(t.deadlineValue?.isOverdue() == false)

        let tomorrow = Deadline.calendar(.current).dateString(Date.now.addingTimeInterval(86_400))
        _ = try await api.todoSetDeadline(date: today, itemID: item.id, deadline: Deadline.allDay(tomorrow), clock: clock)
        let a = try #require(try await listed())
        #expect(a.deadlineValue == .allDay(tomorrow) && a.dueTime == nil)

        _ = try await api.todoSetDeadline(date: today, itemID: item.id, deadline: nil as Deadline?, clock: clock)
        #expect(try await listed() == nil, "cleared")
    }

    @Test func aMissingDocIsNotFound() async throws {
        // today's daemon: 200 {"error": "not found: …"}; the next: a real 404
        await #expect(throws: APIError.self) { try await api.doc(UUID().uuidString.lowercased()) }
        do {
            _ = try await api.doc(UUID().uuidString.lowercased())
        } catch let e as APIError {
            guard case .notFound = e else { Issue.record("not .notFound: \(e)"); return }
        }
    }

    /// Each `DocEditor` op, sent for real: edit → change event → refetch shows it.
    @Test func everyBlockEditRoundTrips() async throws {
        var tree = try await seed("Edit", "# Edit\n\nalpha\n\n## Second\n\nbravo")
        func refetch(after seq: Int, past epoch: Int) async throws -> DocTree {
            let id = tree.doc.id
            _ = try await IT.eventually("change for \(id) past epoch \(epoch)") {
                try await api.changes(since: seq).changes.first { $0.docID == id && ($0.epoch ?? 0) > epoch }
            }
            return try await api.doc(id)
        }
        func send(_ edits: [BlockEdit]) async throws {
            var editor = DocEditor(tree)
            let head = try await api.changesHead()
            let out = try await api.propose(try editor.propose(edits))
            #expect(out.verdicts.allSatisfy { $0.applied && $0.verdict == .green }, "\(edits): \(out.verdicts)")
            tree = try await refetch(after: head, past: tree.doc.currentEpoch)
            // the server's tree and the editor's local view agree
            #expect(DocEditor(tree).ordered().map(\.block.id) == editor.ordered().map(\.block.id))
            #expect(DocEditor(tree).ordered().map(\.block.content) == editor.ordered().map(\.block.content))
        }
        let ed = DocEditor(tree)
        let alpha = try #require(ed.ordered().first { $0.block.content == "alpha" }?.block)
        let second = try #require(ed.ordered().first { $0.block.content == "## Second" }?.block)
        let bravo = try #require(ed.ordered().first { $0.block.content == "bravo" }?.block)

        try await send([.replaceText(alpha.id, "alpha, edited")])
        let newID = UUID().uuidString.lowercased()
        try await send([.insert(after: alpha.id, parent: alpha.parentID, type: .paragraph, content: "inserted", id: newID)])
        #expect(tree.flattened().contains { $0.block.id == newID }, "client-minted id kept")
        try await send([.move(bravo.id, after: nil, parent: alpha.parentID)])
        try await send([.delete(second.id)])
        #expect(!tree.flattened().contains { $0.block.id == second.id })
    }

    /// The app's path: queue offline, replay through the outbox, and let the
    /// SyncEngine pull the result into the cache.
    @Test func outboxReplayLandsThroughSync() async throws {
        let tree = try await seed("Outbox", "# Outbox\n\none")
        let cache = try Cache.inMemory()
        try await cache.storeDoc(tree)
        var editor = try #require(try await cache.editor(for: tree.doc.id))
        let one = try #require(editor.ordered().first { $0.block.content == "one" }?.block)
        try await cache.enqueue([.replaceText(one.id, "one, offline")], on: &editor)
        try await cache.enqueue([.insert(after: one.id, parent: one.parentID, type: .paragraph, content: "two, offline")], on: &editor)

        let sync = SyncEngine(api: api, cache: cache)
        await sync.setAlwaysFetch([tree.doc.id])
        await sync.start()
        defer { Task { await sync.stop() } }
        try await OutboxReplayer(api: api, cache: cache).replay()
        #expect(try await cache.pendingOutbox().isEmpty)
        let contents = try await IT.eventually("cache shows both edits") {
            let c = try await cache.blocks(of: tree.doc.id).map(\.content)
            return c.contains("two, offline") && c.contains("one, offline") ? c : nil
        }
        #expect(contents == ["# Outbox", "one, offline", "two, offline"])
        // chained: the second propose was rebased onto the first's epoch, so both applied
        let latest = try await api.doc(tree.doc.id)
        #expect(latest.doc.currentEpoch == tree.doc.currentEpoch + 2)
    }
}

/// Plays the browser: runs the daemon's softpasskey example, which enrolls a
/// software passkey from the enrollment link, signs in on the authorize URL
/// and prints the redirect carrying the code.
struct SoftPasskeyAuthenticator: WebAuthenticator {
    let binary: String
    let enrollURL: String

    func authenticate(url: URL, callback: OAuthCallback) async throws -> URL {
        let callbackScheme = URL(string: callback.redirectURI)?.scheme
        let p = Process()
        p.executableURL = URL(fileURLWithPath: binary)
        p.arguments = [enrollURL, url.absoluteString]
        let out = Pipe()
        let err = Pipe()
        p.standardOutput = out
        p.standardError = err
        try p.run()
        let data = out.fileHandleForReading.readDataToEndOfFile()
        p.waitUntilExit()
        let text = String(decoding: data, as: UTF8.self).trimmingCharacters(in: .whitespacesAndNewlines)
        guard let line = text.split(separator: "\n").last, let u = URL(string: String(line)),
              u.scheme == callbackScheme
        else {
            let e = String(decoding: err.fileHandleForReading.readDataToEndOfFile(), as: UTF8.self)
            throw ITError.noRedirect("stdout: \(text)\nstderr: \(e)")
        }
        return u
    }
}

/// A fixed bearer, to check what the server does with an old token.
struct FixedToken: TokenProvider {
    let value: String
    func token() async throws -> String? { value }
}

/// Holds the signed-in session across the serialized cases below.
actor ITState {
    static let shared = ITState()
    let store = MemoryTokenStore()
    var firstTokens: TokenSet?
    func setFirst(_ t: TokenSet?) { firstTokens = t }
}

@Suite(.serialized, .enabled(if: IT.server != nil && IT.env["TAISCE_IT_SOFTPASSKEY"] != nil && IT.env["TAISCE_IT_ENROLL_URL"] != nil))
struct ServerIntegrationTests {
    let base: URL
    let oauth: OAuthClient

    init() throws {
        base = try #require(IT.server)
        oauth = OAuthClient(baseURL: base, clientName: "Taisce integration")
    }

    func session(refreshMargin: TimeInterval = 120) -> AuthSession {
        AuthSession(oauth: oauth, store: ITState.shared.store, refreshMargin: refreshMargin)
    }

    func api(_ auth: any TokenProvider) -> APIClient {
        APIClient(config: ServerConfig(baseURL: base, tokenProvider: auth))
    }

    @Test func t1_unauthenticatedIsRefusedWithMetadata() async throws {
        let (_, response) = try await URLSession.shared.data(from: base.appending(path: "api/docs"))
        let http = try #require(response as? HTTPURLResponse)
        #expect(http.statusCode == 401)
        #expect(http.value(forHTTPHeaderField: "WWW-Authenticate")?.contains("resource_metadata=") == true)
        #expect(try await session().requiresAuth())
        await #expect(throws: APIError.unauthorized) { try await api(NoAuth()).tree() }
    }

    @Test func t2_signInRegistersAuthorizesAndExchanges() async throws {
        let auth = session()
        let web = SoftPasskeyAuthenticator(
            binary: try #require(IT.env["TAISCE_IT_SOFTPASSKEY"]),
            enrollURL: try #require(IT.env["TAISCE_IT_ENROLL_URL"])
        )
        try await auth.signIn(using: web)
        #expect(await auth.state == .signedIn)
        let tokens = try #require(try ITState.shared.store.tokens(for: oauth.origin))
        #expect(tokens.expiresAt > .now)
        #expect(try ITState.shared.store.clientID(for: oauth.origin) != nil, "DCR client cached")
        await ITState.shared.setFirst(tokens)
    }

    @Test func t3_authedCallsWork() async throws {
        let api = api(session())
        let doc = try await api.createDoc(title: "Authed \(UUID().uuidString.prefix(6))")
        _ = try await api.proposeMarkdown(ProposeMarkdownRequest(docID: doc.id, baseEpoch: doc.currentEpoch, markdown: "# Authed\n\nhello"))
        let (docs, seq) = try await api.treeWithSeq()
        #expect(docs.contains { $0.id == doc.id } && seq != nil)
        #expect(try await api.doc(doc.id).flattened().count == 2)
        // the SSE stream takes the bearer too
        var gotRetry = false
        for try await out in api.changeStream(lastEventID: seq) {
            if case .retry = out { gotRetry = true; break }
        }
        #expect(gotRetry)
    }

    @Test func t3b_todoCallsCarryTheClock() async throws {
        let api = api(session())
        let clock = TodoClock()
        let day = try await api.todoAdd(date: clock.today, text: "server-mode \(UUID().uuidString.prefix(6))", clock: clock)
        #expect(day.today == clock.today, "the server took the device's today")
        _ = try await api.todoDue()
    }

    @Test func t4_refreshRotatesAndStillWorks() async throws {
        let before = try #require(try ITState.shared.store.tokens(for: oauth.origin))
        // a margin longer than the token's life: every token() call refreshes
        let eager = session(refreshMargin: 10 * 365 * 86400)
        let fresh = try #require(try await eager.token())
        let after = try #require(try ITState.shared.store.tokens(for: oauth.origin))
        #expect(fresh != before.accessToken && after.refreshToken != before.refreshToken)
        #expect(!(try await api(session()).tree()).isEmpty)
    }

    @Test func t5_concurrentRefreshesAreSingleFlight() async throws {
        let before = try #require(try ITState.shared.store.tokens(for: oauth.origin))
        let eager = session(refreshMargin: 10 * 365 * 86400)
        let got = try await withThrowingTaskGroup(of: String?.self) { group in
            for _ in 0..<8 { group.addTask { try await eager.token() } }
            var all: [String?] = []
            for try await t in group { all.append(t) }
            return all
        }
        // eight callers, one rotation: all hold the same new token
        #expect(Set(got).count == 1 && got.first != before.accessToken)
        let after = try #require(try ITState.shared.store.tokens(for: oauth.origin))
        #expect(got.first == after.accessToken)
        // the grant survived (independent refreshes would trip reuse detection)
        #expect(!(try await api(session()).tree()).isEmpty)
        // and the 401 path: a rejected token renews once and retries
        let renewing = api(RenewingStale(stale: "not-a-real-token", real: session()))
        #expect(!(try await renewing.tree()).isEmpty)
    }

    @Test func t6_signOutRevokesTheGrant() async throws {
        let live = try #require(try ITState.shared.store.tokens(for: oauth.origin))
        let auth = session()
        await auth.signOut()
        #expect(await auth.state == .signedOut)
        #expect(try ITState.shared.store.tokens(for: oauth.origin) == nil)
        // the server forgot both tokens
        await #expect(throws: APIError.unauthorized) { try await api(FixedToken(value: live.accessToken)).tree() }
        let d = try #require(try await oauth.discover())
        let client = try #require(try ITState.shared.store.clientID(for: oauth.origin))
        await #expect(throws: AuthError.self) { try await oauth.refresh(live.refreshToken, clientID: client, d) }
    }
}

/// Sends a stale token first, then defers to the real session on renew.
struct RenewingStale: TokenProvider {
    let stale: String
    let real: AuthSession
    func token() async throws -> String? { stale }
    func renew(rejected: String) async throws -> String? { try await real.token() }
}
