import Foundation
import Synchronization
import Testing
import TaisceKit
import UserNotifications
@testable import Taisce

/// A scripted fleet server: one stream per connection, and what was asked.
private final class FakeFleet: FleetService, @unchecked Sendable {
    struct State {
        var fleet: Fleet
        var calls: [String] = []
        /// what each new connection sends before it is held open; nil = 404
        var streams: [[SSEOutput]?] = []
        var open: [AsyncThrowingStream<SSEOutput, any Error>.Continuation] = []
        var missing = false
        /// `fleet()` waits here until released (a slow answer)
        var held: [CheckedContinuation<Void, Never>] = []
        var hold = false
        var unauthorized = false
    }

    let state: Mutex<State>
    init(_ fleet: Fleet) { state = Mutex(State(fleet: fleet)) }

    var calls: [String] { state.withLock { $0.calls } }

    static func event(_ name: String, rev: Int, _ value: some Encodable) -> SSEOutput {
        let data = String(decoding: (try? JSONEncoder().encode(value)) ?? Data(), as: UTF8.self)
        return .event(SSEEvent(id: String(rev), event: name, data: data))
    }

    /// Send on every open connection.
    func send(_ output: SSEOutput) { state.withLock { s in s.open.forEach { $0.yield(output) } } }

    /// Let the held `fleet()` calls answer.
    func release() {
        let waiting = state.withLock { s in s.hold = false; defer { s.held = [] }; return s.held }
        waiting.forEach { $0.resume() }
    }

    func fleet() async throws -> Fleet {
        if state.withLock({ $0.hold }) {
            await withCheckedContinuation { c in state.withLock { $0.held.append(c) } }
        }
        return try state.withLock { s in
            s.calls.append("fleet")
            if s.missing { throw APIError.notFound("not found") }
            return s.fleet
        }
    }

    func fleetStream() -> AsyncThrowingStream<SSEOutput, any Error> {
        AsyncThrowingStream { c in
            let script: [SSEOutput]? = state.withLock { s in
                s.calls.append("stream")
                if s.missing { return nil }
                let next = s.streams.isEmpty ? [Self.event("fleet", rev: s.fleet.rev, s.fleet)] : s.streams.removeFirst()
                if next != nil { s.open.append(c) }
                return next
            }
            if state.withLock({ $0.unauthorized }) { return c.finish(throwing: APIError.unauthorized) }
            guard let script else { return c.finish(throwing: APIError.http(status: 404)) }
            script.forEach { c.yield($0) }
        }
    }

    func snoozeSession(_ key: FleetSessionKey, until: Date) async throws -> FleetSession {
        try change(key, "snooze \(key.id) \(Int(until.timeIntervalSince1970))", Int(until.timeIntervalSince1970))
    }

    func unsnoozeSession(_ key: FleetSessionKey) async throws -> FleetSession {
        try change(key, "unsnooze \(key.id)", nil)
    }

    private func change(_ key: FleetSessionKey, _ call: String, _ until: Int?) throws -> FleetSession {
        try state.withLock { s in
            s.calls.append(call)
            guard let i = s.fleet.sessions.firstIndex(where: { $0.key == key }) else { throw APIError.notFound("not found") }
            s.fleet.sessions[i].quietUntil = until
            return s.fleet.sessions[i]
        }
    }
}

/// The Agents tab (Agent Fabric, Slice 1: Watch): what the screen says, the
/// store behind it, where it sits in the app, and its notifications.
@MainActor @Suite(.serialized, .timeLimit(.minutes(1))) struct FleetTests {
    /// 2026-11-20 12:10 UTC
    static let now = Date(timeIntervalSince1970: 1_795_176_600)
    static var nowS: Int { Int(now.timeIntervalSince1970) }
    static var utc: Calendar {
        var c = Calendar(identifier: .gregorian)
        c.timeZone = TimeZone(identifier: "UTC") ?? .gmt
        return c
    }
    static let en = Locale(identifier: "en_IE")

    static func session(_ id: String, _ address: String, _ state: SessionState, runtime: String = "r1", account: String = "personal",
                        tool: String = "", note: String = "", last: String = "", context: Int? = nil, ago: Int = 300, quiet: Int? = nil) -> FleetSession {
        FleetSession(sessionID: id, runtimeID: runtime, address: address, branch: "main", account: account, state: state, tool: tool,
                     note: note, lastLine: last, contextUsedPct: context, since: nowS - ago, updatedAt: nowS - ago, quietUntil: quiet)
    }

    static func runtime(_ id: String = "r1", name: String = "mbp", online: Bool = true, seen: Int? = nowS, accounts: [FleetAccount] = [], at: Int? = nil) -> FleetRuntime {
        FleetRuntime(id: id, name: name, online: online, lastSeenAt: seen, accounts: accounts, accountsAt: at)
    }

    static let fleet = Fleet(rev: 5, runtimes: [runtime()], sessions: [
        session("s1", "taisce/main", .needsYou, note: "running the suite", last: "Shall I open the pull request?", context: 44),
        session("s2", "qompass/mcp-dx", .stalled, last: "Password:"),
        session("s3", "workbox/main", .finished, account: "work", last: "Done."),
        session("s4", "taisce/fabric", .working, tool: "Bash", note: "building the app", last: "ok", context: 91, ago: 7300),
        session("s5", "notes/main", .idle),
        session("s6", "old/main", .exited),
    ])

    func board(_ fleet: Fleet) -> FleetBoard { FleetBoard(fleet, now: Self.now, calendar: Self.utc, locale: Self.en) }

    // MARK: what the screen says

    @Test func rowsKeepTheServersOrderAndRestingOnesFoldAway() {
        let b = board(Self.fleet)
        #expect(b.rows.map(\.session.address) == ["taisce/main", "qompass/mcp-dx", "workbox/main", "taisce/fabric"])
        #expect(b.resting.map(\.session.address) == ["notes/main", "old/main"])
        #expect(b.summary == "2 need you \u{00B7} 1 working")
        #expect(!b.hasNoRuntime && !b.hasNoSessions)
        // a state this build has no name for is shown among the active ones, in place
        var odd = Self.fleet
        odd.sessions[4].state = .unknown
        #expect(board(odd).rows.map(\.session.address).last == "notes/main")
        #expect(board(odd).rows.last?.status == "Unknown state \u{00B7} 5 min")
    }

    @Test func aRowSaysItsStateInWordsWhatItWaitsOnAndWhatItRunsOn() {
        let rows = board(Self.fleet).rows
        // waiting on you: the question, not the note
        #expect(rows[0].status == "Needs you \u{00B7} 5 min" && rows[0].detail == "Shall I open the pull request?")
        #expect(rows[0].meta == "personal \u{00B7} main \u{00B7} context 44%" && !rows[0].contextNearlyFull && rows[0].tone == .yellow)
        #expect(rows[1].tone == .red && rows[2].tone == .green && rows[2].detail == "Done.")
        // in a turn: the tool, how long, and the note
        #expect(rows[3].status == "Working \u{00B7} Bash \u{00B7} 2 h" && rows[3].detail == "building the app")
        #expect(rows[3].contextNearlyFull && rows[3].tone == .accent)
        #expect(rows[3].spoken == "taisce/fabric, Working \u{00B7} Bash \u{00B7} 2 h, building the app, personal \u{00B7} main \u{00B7} context 91%")
        // nothing reported: no empty lines
        let bare = FleetRow(FleetSession(sessionID: "x", runtimeID: "r1", address: "a/b", state: .idle, since: Self.nowS, updatedAt: Self.nowS), offline: false, now: Self.now)
        #expect(bare.status == "Idle \u{00B7} now" && bare.detail == nil && bare.meta == nil && bare.snoozed == nil)
        #expect(SessionState.allCases.map(\.words) == ["Needs you", "Stalled", "Finished", "Waiting on a peer", "Working", "Unknown state", "Idle", "Asleep", "Exited"])
        #expect(FleetTime.elapsed(since: Self.nowS - 59, now: Self.now) == "now")
        #expect(FleetTime.elapsed(since: Self.nowS - 3 * 86400, now: Self.now) == "3 d")
        #expect(FleetTime.elapsed(since: Self.nowS + 50, now: Self.now) == "now", "a runtime's clock a little ahead")
    }

    @Test func anOfflineRuntimeSaysMacAsleepAndItsSessionsAreLastKnown() {
        var away = Self.fleet
        away.runtimes = [Self.runtime(online: false, seen: Self.nowS - 1740)]
        let b = board(away)
        #expect(b.runtimes.map(\.title) == ["Mac asleep since 11:41"])
        #expect(b.runtimes[0].detail == "mbp \u{00B7} sessions shown as last known" && !b.runtimes[0].online)
        #expect(b.rows.allSatisfy(\.offline) && b.rows[0].tone == .neutral)
        #expect(b.rows[3].status == "Working \u{00B7} Bash \u{00B7} last known", "no clock runs on a Mac that is away")
        #expect(b.summary == nil && away.attentionCount(at: Self.now) == 0)
        // a second Mac that is awake: only the first one's rows are dimmed
        away.runtimes.append(Self.runtime("r2", name: "studio"))
        away.sessions.append(Self.session("s9", "site/main", .working, runtime: "r2"))
        let two = board(away)
        #expect(two.runtimes.map(\.title) == ["Mac asleep since 11:41", "Mac awake"])
        #expect(two.rows.filter { !$0.offline }.map(\.session.address) == ["site/main"])
        // seen on another day; never seen
        #expect(FleetRuntimeNote(Self.runtime(online: false, seen: Self.nowS - 2 * 86400), now: Self.now, calendar: Self.utc, locale: Self.en).title == "Mac asleep since Wed 12:10")
        let never = FleetRuntimeNote(Self.runtime(online: false, seen: nil), now: Self.now, calendar: Self.utc, locale: Self.en)
        #expect(never.title == "Mac not connected yet" && never.detail == "mbp \u{00B7} enrolled, never seen")
    }

    @Test func headroomSaysWhatIsLeftAndWarnsOnlyWhereSessionsRun() {
        var f = Self.fleet
        f.runtimes = [Self.runtime(accounts: [
            FleetAccount(name: "personal", sessionUsedPct: 38, sessionResetsAt: Self.nowS + 6900, weekUsedPct: 61, weekResetsAt: Self.nowS + 3 * 86400),
            FleetAccount(name: "work", sessionUsedPct: 92),
            FleetAccount(name: "spare", weekUsedPct: 99, weekResetsAt: Self.nowS - 5),
            FleetAccount(name: "blank"),
        ], at: 10)]
        var h = board(f).headroom
        #expect(h.map(\.name) == ["personal", "work", "spare", "blank"])
        #expect(h[0].session == "session 62% left \u{00B7} resets 14:05" && h[0].week == "week 39% left \u{00B7} resets Mon 12:10" && !h[0].warning)
        // nearly spent, but its only session has finished its turn and is not at rest: it counts as running
        #expect(h[1].session == "session 8% left" && h[1].week == nil && h[1].warning)
        // nearly spent with nothing running on it: no warning; a reset already past is not said
        #expect(h[2].week == "week 1% left" && !h[2].warning)
        #expect(h[3].session == nil && h[3].week == nil && h[3].spoken == "blank")
        // the same account from two Macs: the later report, once
        f.runtimes.append(Self.runtime("r2", name: "studio", accounts: [FleetAccount(name: "work", sessionUsedPct: 20)], at: 99))
        h = board(f).headroom
        #expect(h.map(\.name) == ["personal", "work", "spare", "blank"] && h[1].session == "session 80% left")
    }

    @Test func emptyFleets() {
        #expect(board(Fleet()).hasNoRuntime)
        let enrolled = board(Fleet(rev: 1, runtimes: [Self.runtime(online: false, seen: nil)]))
        #expect(!enrolled.hasNoRuntime && enrolled.hasNoSessions && enrolled.summary == nil)
        // Today's card: nothing without a fleet or a Mac; otherwise what needs you
        #expect(FleetCard(Fleet(), available: true) == nil && FleetCard(Self.fleet, available: false) == nil)
        let card = FleetCard(Self.fleet, available: true, now: Self.now)
        #expect(card?.subtitle == "2 need you \u{00B7} 1 working" && card?.attention == true)
        let quiet = Fleet(rev: 1, runtimes: [Self.runtime()], sessions: [Self.session("s5", "notes/main", .idle)])
        #expect(FleetCard(quiet, available: true)?.subtitle == "1 session, none needs you" && FleetCard(quiet, available: true)?.attention == false)
        #expect(FleetCard(Fleet(rev: 1, runtimes: [Self.runtime(online: false)]), available: true)?.subtitle == "Mac asleep")
    }

    @Test func snoozeLastsAnHourOrUntilTheNextMorningAndTheRowSaysUntilWhen() {
        #expect(FleetSnooze.allCases.map { $0.title(now: Self.now, calendar: Self.utc) } == ["For an hour", "Until tomorrow"])
        #expect(FleetSnooze.oneHour.until(now: Self.now, calendar: Self.utc) == Self.now.addingTimeInterval(3600))
        // 09:00 the next day on this device's clock
        let morning = FleetSnooze.nextMorning.until(now: Self.now, calendar: Self.utc)
        #expect(Int(morning.timeIntervalSince1970) == 1_795_251_600)
        // in the small hours the next morning is today's, and the label says so
        let small = Self.now.addingTimeInterval(13 * 3600) // 01:10 on the 21st
        #expect(FleetSnooze.nextMorning.title(now: small, calendar: Self.utc) == "Until this morning")
        #expect(FleetSnooze.nextMorning.until(now: small, calendar: Self.utc) == morning, "under 8 hours away, not 32")
        let five = Self.now.addingTimeInterval(17 * 3600) // 05:10: the day has begun
        #expect(FleetSnooze.nextMorning.title(now: five, calendar: Self.utc) == "Until tomorrow")
        #expect(FleetSnooze.nextMorning.until(now: five, calendar: Self.utc) == morning.addingTimeInterval(86400))
        var f = Self.fleet
        f.sessions[0].quietUntil = Self.nowS + 3600
        f.sessions[1].quietUntil = Int(morning.timeIntervalSince1970)
        f.sessions[2].quietUntil = Self.nowS - 1
        let b = board(f)
        let rows = b.rows
        #expect(rows[0].snoozed == "Snoozed until 13:10" && rows[1].snoozed == "Snoozed until Sat 09:00")
        #expect(rows[2].snoozed == nil, "a snooze that has passed is over, whether or not the server has said so yet")
        // snoozing hides nothing
        #expect(rows.count == 4 && rows[0].status == "Needs you \u{00B7} 5 min")
        #expect(rows[0].spoken.hasSuffix("Snoozed until 13:10"))
        // but a snoozed session is no longer counted as needing you: the summary, the badge, Today's card
        #expect(b.summary == "1 working" && f.attentionCount(at: Self.now) == 0)
        #expect(FleetCard(f, available: true, now: Self.now)?.attention == false)
        #expect(FleetCard(Self.fleet, available: true, now: Self.now)?.attention == true && Self.fleet.attentionCount(at: Self.now) == 2)
    }

    @Test func timesAreWrittenAsTheLocaleWritesThem() {
        let us = Locale(identifier: "en_US")
        // a 12-hour clock keeps its AM/PM (11:41 alone could be either)
        let morning = FleetTime.clock(Self.nowS - 1740, now: Self.now, calendar: Self.utc, locale: us)
        #expect(morning.hasPrefix("11:41") && morning.hasSuffix("AM"), "\(morning)")
        let evening = FleetTime.clock(Self.nowS + 6 * 3600, now: Self.now, calendar: Self.utc, locale: us)
        #expect(evening.hasPrefix("6:10") && evening.hasSuffix("PM"), "\(evening)")
        let other = FleetTime.clock(Self.nowS - 2 * 86400, now: Self.now, calendar: Self.utc, locale: us)
        #expect(other.hasPrefix("Wed") && other.hasSuffix("PM"), "\(other)")
        // a 24-hour one has none
        #expect(FleetTime.clock(Self.nowS - 1740, now: Self.now, calendar: Self.utc, locale: Self.en) == "11:41")
        #expect(FleetTime.clock(Self.nowS + 6 * 3600, now: Self.now, calendar: Self.utc, locale: Self.en) == "18:10")
        #expect(FleetTime.clock(Self.nowS - 30 * 86400, now: Self.now, calendar: Self.utc, locale: Self.en) == "21 Oct")
    }

    // MARK: the store

    private func store(_ fake: FakeFleet) -> FleetStore {
        let s = FleetStore()
        s.serviceOverride = fake
        s.backoff = Backoff(base: .milliseconds(5), cap: .milliseconds(5))
        return s
    }

    func until(_ what: String, _ check: @MainActor () -> Bool) async {
        for _ in 0..<400 where !check() { try? await Task.sleep(for: .milliseconds(5)) }
        #expect(check(), "\(what)")
    }

    @Test func theStoreFollowsTheStreamAndKeepsRowsStill() async {
        let fake = FakeFleet(Self.fleet)
        let s = store(fake)
        #expect(s.availability == .unknown && !s.isLoaded)
        s.start()
        s.start()
        await until("the fleet arrives") { s.isLoaded }
        #expect(s.fleet == Self.fleet && s.link == .live && s.loadError == nil)
        #expect(fake.calls == ["stream"], "asked to start twice, it follows once")
        // the last row comes to need you: it changes where it stands
        var asks = Self.fleet.sessions[3]
        asks.state = .needsYou
        fake.send(FakeFleet.event("session", rev: 6, asks))
        await until("the change lands") { s.fleet.rev == 6 }
        #expect(s.fleet.sessions.map(\.address) == Self.fleet.sessions.map(\.address))
        #expect(s.fleet.sessions[3].state == .needsYou && s.fleet.attentionCount() == 3)
        // the Mac goes away: nothing needs you now, everything is still listed
        fake.send(FakeFleet.event("runtime", rev: 7, Self.runtime(online: false)))
        await until("offline") { s.fleet.rev == 7 }
        #expect(s.fleet.attentionCount() == 0 && s.fleet.sessions.count == 6)
        // a refresh brings the server's order back
        fake.state.withLock { $0.fleet = Fleet(rev: 8, runtimes: [Self.runtime()], sessions: [asks] + Self.fleet.sessions.prefix(3)) }
        await s.refresh()
        #expect(s.fleet.rev == 8 && s.fleet.sessions.first?.address == "taisce/fabric")
        s.stop()
        #expect(s.link == .idle && s.isLoaded, "backgrounded: what was last seen stays")
        s.reset()
        #expect(s.fleet == Fleet() && s.availability == .unknown)
    }

    @Test func aServerWithNoFleetIsNotAnError() async {
        let fake = FakeFleet(Fleet())
        fake.state.withLock { $0.missing = true }
        let s = store(fake)
        s.start()
        await until("the server says there is none") { s.availability == .unavailable }
        #expect(s.loadError == nil && s.link == .unavailable && s.fleet == Fleet())
        try? await Task.sleep(for: .milliseconds(40))
        #expect(fake.calls == ["stream"], "asked once, not again and again")
        // the screen's own refresh reads the same answer the same way
        let other = store(fake)
        await other.refresh()
        #expect(other.availability == .unavailable && other.loadError == nil)
        #expect(fake.calls == ["stream", "fleet"], "a refresh reads; it never starts the following")
        #expect(!other.isOffered && FleetStore().isOffered, "offered until the server is known to have none")
    }

    @Test func aDroppedStreamReconnectsAndAFailureBeforeAnyFleetIsSaid() async {
        let fake = FakeFleet(Self.fleet)
        // the first connection dies with nothing; the second brings the fleet
        fake.state.withLock { $0.streams = [[]] }
        let s = store(fake)
        s.start()
        await until("connected") { fake.calls.count == 1 }
        fake.state.withLock { st in st.open.forEach { $0.finish(throwing: URLError(.networkConnectionLost)) }; st.open = [] }
        await until("the fleet after a reconnect") { s.isLoaded }
        #expect(fake.calls == ["stream", "stream"] && s.loadError == nil && s.link == .live)
        s.stop()
    }

    @Test func snoozeMarksTheRowAtOnceAndSaysWhyWhenItFails() async {
        let fake = FakeFleet(Self.fleet)
        let s = store(fake)
        await s.refresh()
        let key = Self.fleet.sessions[0].key
        #expect(await s.snooze(key, .oneHour, now: Self.now) == nil)
        #expect(s.fleet.sessions[0].quietUntil == Self.nowS + 3600)
        #expect(fake.calls.contains("snooze s1 \(Self.nowS + 3600)"))
        #expect(s.fleet.sessions.count == 6, "snoozed, not hidden")
        #expect(await s.unsnooze(key) == nil)
        #expect(s.fleet.sessions[0].quietUntil == nil && fake.calls.last == "unsnooze s1")
        #expect(await s.snooze(FleetSessionKey(runtimeID: "r1", id: "gone"), .nextMorning) == "That session is gone.")
        let none = FleetStore()
        #expect(await none.snooze(key, .oneHour) == "Not connected.")
        s.stop()
    }

    @Test func anAnswerForTheAccountThatSignedOutIsDropped() async {
        let fake = FakeFleet(Self.fleet)
        let s = store(fake)
        fake.state.withLock { $0.hold = true }
        let refresh = Task { await s.refresh() }
        await until("the refresh is asked") { fake.state.withLock { !$0.held.isEmpty } }
        // sign-out (or another server) while the answer is on its way
        s.reset()
        fake.release()
        await refresh.value
        #expect(s.fleet == Fleet() && s.availability == .unknown && s.loadError == nil, "the last account's fleet did not come back")
        #expect(fake.calls == ["fleet"], "and nothing started following")
        // the same for a snooze answered late
        await s.refresh()
        #expect(s.isLoaded)
        s.reset()
        #expect(await s.snooze(Self.fleet.sessions[0].key, .oneHour) == nil)
        #expect(s.fleet.sessions.isEmpty)
    }

    @Test func aRefusedTokenEndsTheFollowingUntilTheNextStart() async {
        let fake = FakeFleet(Self.fleet)
        fake.state.withLock { $0.unauthorized = true }
        let s = store(fake)
        s.start()
        await until("the follower gave up") { s.loadError != nil && s.link == .idle }
        #expect(s.loadError == "Sign in again to see your agents." && !s.isLoaded)
        try? await Task.sleep(for: .milliseconds(40))
        #expect(fake.calls == ["stream"], "not retried")
        // signed in again: the app's next start follows afresh
        fake.state.withLock { $0.unauthorized = false }
        s.start()
        await until("the fleet arrives") { s.isLoaded }
        s.stop()
    }

    // MARK: where it sits, and notifications

    @Test func agentsIsInTheSidebarAndOverTodayOnAPhone() {
        let key = FleetSessionKey(runtimeID: "r1", id: "s1")
        let phone = Router()
        phone.tab = .library
        phone.showAgents(focus: key)
        #expect(phone.tab == .today && phone.todayPath == [.agents] && phone.agentsFocus == key)
        let pad = Router()
        pad.isPad = true
        pad.select(.doc("d1"))
        pad.showAgents()
        #expect(pad.padItem == .agents && pad.padPath.isEmpty && pad.sidebarSelection == .agents && pad.agentsFocus == nil)
        // already there: a second ask is not another place to go back to
        let places = pad.backPlaces.count
        pad.showAgents(focus: key)
        #expect(pad.backPlaces.count == places && pad.agentsFocus == key)
        #expect(pad.perform(.agents, picker: WorkspacePicker(workspaces: [], unsortedCount: 0, stored: nil)) == .none)
        #expect(ActionsPalette.all.contains { $0.command == .agents })
        // a server known to have no fleet (a LOCAL daemon): not offered in the menu or the palette
        let picker = WorkspacePicker(workspaces: [], unsortedCount: 0, stored: nil)
        #expect(pad.canPerform(.agents, signedIn: true, picker: picker) && !pad.canPerform(.agents, signedIn: true, picker: picker, agents: false))
        #expect(!pad.canPerform(.agents, signedIn: false, picker: picker))
        #expect(!ActionsPalette.matching("agents") { pad.canPerform($0, signedIn: true, picker: picker, agents: false) }.contains { $0.command == .agents })
        #expect(ActionsPalette.matching("agents") { pad.canPerform($0, signedIn: true, picker: picker) }.map(\.command) == [.agents])
    }

    @Test func aSessionNotificationIsReadAsTheServerSendsIt() {
        // crates/daemon/src/push.rs, Push::FabricSession
        let info: [AnyHashable: Any] = [
            "aps": ["alert": ["title": "taisce/main", "body": "Needs you"], "sound": "default", "thread-id": "fabric:0196f3a0-0000-7000-8000-000000000001"],
            "kind": "fabric_session", "session_id": "0196f3a0-0000-7000-8000-000000000001",
            "runtime_id": "01a1fb15-5eea-78d0-a6b7-adbb1135747c", "state": "needs_you",
        ]
        #expect(FabricSessionPush.session(from: info) == FleetSessionKey(runtimeID: "01a1fb15-5eea-78d0-a6b7-adbb1135747c", id: "0196f3a0-0000-7000-8000-000000000001"))
        #expect(FabricSessionPush.session(from: ["kind": "share_comment", "doc_id": "d1"]) == nil)
        #expect(FabricSessionPush.session(from: ["kind": "fabric_session", "session_id": "s"]) == nil)
        #expect(ShareCommentPush.docID(from: info) == nil)
        // in front of the Agents screen there is nothing to announce; anywhere else it shows like any other
        #expect(FabricSessionPush.presentation(agentsOnScreen: true) == [])
        #expect(FabricSessionPush.presentation(agentsOnScreen: false) == [.banner, .list, .sound])
    }

    @Test func aTappedNotificationWaitsForAWindow() {
        let s = FleetStore()
        let key = FleetSessionKey(runtimeID: "r1", id: "s1")
        s.requestSession(key)
        #expect(s.request == FleetSessionRequest(session: key, serial: 1))
        #expect(s.takeRequest() == key && s.request == nil && s.takeRequest() == nil)
    }

    /// The blocker from review: a screen that was removed (a sidebar switch,
    /// a pop) stayed counted as "in front", and every later session
    /// notification was swallowed.
    @Test func aScreenThatHasGoneIsNoLongerCountedAsInFront() {
        let s = FleetStore()
        let screen = FleetScreenPresence()
        #expect(!s.isOnScreen)
        #expect(screen.update(inFront: true, in: s), "it came to the front")
        #expect(s.isOnScreen && FabricSessionPush.presentation(agentsOnScreen: s.isOnScreen) == [])
        // said twice (the view's change handler and its appearance): counted once
        #expect(!screen.update(inFront: true, in: s))
        #expect(s.watching == 1)
        // the view is removed: its onDisappear says so, with no change of state to observe
        screen.update(inFront: false, in: s)
        screen.update(inFront: false, in: s)
        #expect(!s.isOnScreen && s.watching == 0)
        #expect(FabricSessionPush.presentation(agentsOnScreen: s.isOnScreen) == [.banner, .list, .sound])
        // two windows: one leaving does not hide the other
        let (one, two) = (FleetScreenPresence(), FleetScreenPresence())
        one.update(inFront: true, in: s)
        two.update(inFront: true, in: s)
        one.update(inFront: false, in: s)
        #expect(s.isOnScreen)
        two.update(inFront: false, in: s)
        #expect(!s.isOnScreen)
    }
}
