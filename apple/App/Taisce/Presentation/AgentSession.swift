import Foundation
import Observation
import TaisceKit

// A session's screen (Slices 2–3): its header, its turns, what is queued,
// and what the reply box does. Pure where it can be; the one store is the
// feed of turns, paged from the server and topped up by the stream.

/// When the person last looked at each session (the newest turn's end they
/// saw), on this device: what "N new since you looked" counts from.
@MainActor
final class LastLooked {
    static let shared = LastLooked()
    private let key = "fabric.lastLooked"
    private var map: [String: Int]

    init() {
        map = (UserDefaults.standard.dictionary(forKey: key) as? [String: Int]) ?? [:]
    }

    private static func id(_ k: FleetSessionKey) -> String { "\(k.runtimeID)/\(k.id)" }

    func seen(_ k: FleetSessionKey) -> Int? { map[Self.id(k)] }

    func look(_ k: FleetSessionKey, upTo ended: Int) {
        guard ended > (map[Self.id(k)] ?? 0) else { return }
        map[Self.id(k)] = ended
        // a week of sessions is plenty to remember
        if map.count > 500 { map = Dictionary(uniqueKeysWithValues: map.sorted { $0.value > $1.value }.prefix(300).map { ($0.key, $0.value) }) }
        UserDefaults.standard.set(map, forKey: key)
    }
}

/// The header: address, what it runs on, its state.
struct SessionHeader: Hashable {
    var address: String
    /// "personal · agent-fabric · context 44%"
    var meta: String
    /// "Needs you · 2 min"
    var status: String
    var tone: LapisTone
    /// "on mbp", "on mbp · asleep since 12:41"
    var place: String
    var contextNearlyFull: Bool

    init(_ s: FleetSession, runtime: FleetRuntime?, now: Date) {
        address = s.address
        var m: [String] = []
        if !s.account.isEmpty { m.append(s.account) }
        if !s.branch.isEmpty { m.append(s.branch) }
        if let pct = s.contextUsedPct { m.append("context \(pct)%") }
        meta = m.joined(separator: " \u{00B7} ")
        let row = FleetRow(s, offline: !(runtime?.online ?? false), now: now)
        status = row.status
        tone = row.tone
        contextNearlyFull = row.contextNearlyFull
        if let runtime {
            place = runtime.online ? "on \(runtime.name)" : "on \(runtime.name) \u{00B7} \(FleetRuntimeNote(runtime, now: now).title.lowercased())"
        } else {
            place = ""
        }
    }
}

/// One turn as the feed shows it. Its Markdown is parsed here, once per
/// turn (the feed keeps its rows), never in a view's `body`; and only as
/// much as the collapsed turn shows: a long reply's whole text is parsed
/// when the person asks for it (`SessionFeed.expand`). `Sendable`, so the
/// feed builds rows off the main actor when a session opens.
struct TurnRow: Identifiable, Hashable, Sendable {
    /// a reply longer than this shows its start (cut at a block boundary) until "Show all"
    static let collapsedLength = 900

    var id: String
    var prompt: String
    /// the prompt as shown: the person's words, or what the Mac started the turn with
    var promptDisplay: PromptDisplay
    var reply: String
    /// "7 tools · 2 files · +6 −2 · 1 commit"
    var activity: String?
    /// "from your phone", "your answer"
    var source: String?
    /// "Asked you", "Stopped"
    var ending: String?
    var endTone: LapisTone
    var when: String
    var pullRequest: URL?
    var textOff: Bool
    var truncated: Bool
    /// what the collapsed turn shows: the whole reply when it is short, else its start
    let replyNodes: [ReplyNode]
    /// the reply is longer than its collapsed start ("Show all")
    let long: Bool

    init(_ t: FabricTurn, now: Date) {
        id = t.turnID
        prompt = t.prompt
        promptDisplay = PromptDisplay(prompt: t.prompt, source: t.source)
        reply = t.reply
        var a: [String] = []
        if t.toolCount > 0 { a.append(t.toolCount == 1 ? "1 tool" : "\(t.toolCount) tools") }
        if let o = FabricWords.outcome(t.outcome) { a.append(o) }
        activity = a.isEmpty ? nil : a.joined(separator: " \u{00B7} ")
        // a turn the Mac started says so in its own line, not under a bubble
        source = promptDisplay.words.isEmpty && !promptDisplay.notes.isEmpty ? nil : FabricWords.source(t.source)
        ending = FabricWords.turnEnd(t.end)
        endTone = switch t.end {
        case .question: .yellow
        case .error, .denied: .red
        case .interrupted: .neutral
        default: .green
        }
        when = FleetTime.clock(t.ended, now: now)
        pullRequest = t.outcome.pullRequestURL.flatMap(URL.init(string:))
        textOff = t.textOff && t.prompt.isEmpty && t.reply.isEmpty
        truncated = t.truncated
        if let start = ReplyMarkdown.preview(t.reply, limit: Self.collapsedLength) {
            replyNodes = ReplyMarkdown.parse(start)
            long = true
        } else {
            replyNodes = ReplyMarkdown.parse(t.reply)
            long = false
        }
    }

    /// Whether this row still says what `t` says on `now`'s day (a reply
    /// only grows, from the route's to the live event's: its length tells).
    func shows(_ t: FabricTurn, now: Date) -> Bool {
        reply.utf8.count == t.reply.utf8.count && prompt.utf8.count == t.prompt.utf8.count
            && textOff == (t.textOff && t.prompt.isEmpty && t.reply.isEmpty) && when == FleetTime.clock(t.ended, now: now)
    }

    // the parsed text follows from `reply`: comparing it again would only cost
    static func == (a: TurnRow, b: TurnRow) -> Bool {
        a.id == b.id && a.prompt == b.prompt && a.reply == b.reply && a.activity == b.activity && a.source == b.source
            && a.ending == b.ending && a.endTone == b.endTone && a.when == b.when && a.pullRequest == b.pullRequest
            && a.textOff == b.textOff && a.truncated == b.truncated
    }

    func hash(into h: inout Hasher) {
        h.combine(id)
        h.combine(reply.count)
        h.combine(when)
    }
}

/// What the reply box does for this session now.
enum ComposerMode: Equatable {
    /// idle, finished or asking: the words are the next prompt
    case reply
    /// a turn is running (or the queue is not empty): the words wait their turn
    case queue
    /// no process: resume it first
    case asleep
    /// the Mac is away: it will get the words when it wakes
    case offline
    /// the Mac is awake but its runtime closed its stream (no sessions):
    /// the words wait at the server until it connects again
    case idle
    /// Hold all is on
    case held
    /// exited
    case gone

    init(_ s: FleetSession, runtime: FleetRuntime?) {
        if runtime?.held ?? false { self = .held; return }
        switch s.state {
        case .exited: self = .gone
        case .asleep: self = .asleep
        case .working, .stalled, .waitingOnPeer: self = .queue
        default: self = s.queue.isEmpty ? .reply : .queue
        }
        if self == .reply || self == .queue, !(runtime?.online ?? false) { self = runtime?.connection == .idle ? .idle : .offline }
    }

    var placeholder: String {
        switch self {
        case .reply: "Reply"
        case .queue: "Queue a message"
        case .asleep: "Resume it to reply"
        case .offline: "Your Mac is asleep: it gets this when it wakes"
        case .idle: "Your Mac's runtime is closed: it gets this when it connects"
        case .held: "Hold all is on"
        case .gone: "This session has exited"
        }
    }

    var canType: Bool { self == .reply || self == .queue || self == .offline || self == .idle }
    /// a send queues behind whatever runs rather than being refused when the turn moved on
    var queues: Bool { self == .queue }
}

/// Replies a tap away when the session is waiting on you and asked no
/// question of its own (a question's choices replace these).
enum QuickReplies {
    static let stock = ["Yes, go ahead", "Not yet", "Explain first", "Show me the diff"]
}


/// The feed of one session's turns.
///
/// Opening: what this device cached is read first, always (off the main
/// actor, with the rows for the newest turns built there too), then the
/// stream's turns from this app run are merged, then the server is asked
/// only for what ended after the cache's watermark (`Cache.CachedTurns.synced`:
/// the newest end up to which what is held is gap-free), page by page. A
/// hole the stream left (the app was away) is so fetched, never skipped.
///
/// The window: only the newest `window` turns are drawn at first; "Earlier
/// turns" shows the next `window` held in memory at once, and reads the
/// server (`before=`) only when nothing older is held. Rows are built (their
/// Markdown parsed) only for the turns drawn. Turns never change once ended,
/// so what is cached stays true; whatever this feed reads is cached too.
@MainActor @Observable
final class SessionFeed {
    /// turns drawn when a session opens, and shown more per "Earlier turns"
    static let window = 20

    let key: FleetSessionKey
    /// every turn held, oldest first
    private(set) var turns: [FabricTurn] = []
    /// the drawn turns (the window), each built once and kept while its turn and its day are unchanged
    private(set) var rows: [TurnRow] = []
    /// the clock rows are worded against (a test's fixed one)
    @ObservationIgnored var now: () -> Date = Date.init
    /// a result that comes back after sign-out (or another server) is not
    /// this account's: nothing is cached from it (`FleetStore.feed` sets it)
    @ObservationIgnored var stillCurrent: @MainActor () -> Bool = { true }
    /// the server holds turns older than the oldest held
    private(set) var more = false
    /// the cache has been read (or there is none): the screen draws from
    /// here, never an empty state first
    private(set) var opened = false
    /// the newest turns are being asked for
    private(set) var loading = false
    /// an older page is being read
    private(set) var loadingOlder = false
    /// why there is nothing to show (the first load failed with nothing held)
    private(set) var loadError: String?
    /// something went wrong with turns already showing: said once (a toast)
    private(set) var problem: FabricNotice?
    /// the newest turn's end the person had seen when the screen opened
    private(set) var seenUpTo: Int?
    /// the feed opened from this device's cache (before the network answered)
    private(set) var fromCache = false
    /// the newest end up to which `turns` is every turn the server has
    private(set) var synced: Int?
    /// the oldest turn drawn (nil: all of them, while they fit the window)
    private(set) var floor: String?
    /// the first drawn turn the person has not seen (where "N new" sits), and how many are new
    private(set) var firstNew: String?
    private(set) var newCount = 0
    /// long replies shown whole, by turn id: their whole text, parsed once
    private(set) var expanded: [String: [ReplyNode]] = [:]
    /// rows built for turns drawn now or lately (a window moved back keeps them)
    @ObservationIgnored private var built: [String: TurnRow] = [:]
    /// where a top-up starts when the cache had no watermark (its oldest turn)
    @ObservationIgnored private var cachedOldest: Int?

    init(key: FleetSessionKey) {
        self.key = key
        seenUpTo = LastLooked.shared.seen(key)
    }

    /// Open from the cache (once), merge the stream's turns, then fetch what is newer.
    func load(_ service: (any FabricService)?, cache: Cache? = nil, live: [FabricTurn] = []) async {
        await open(cache)
        merge(live)
        await refresh(service, cache: cache)
    }

    /// Read what this device holds, once: the turns and the rows for the
    /// window are made off the main actor, so the first frame waits on
    /// neither the database nor Markdown. Already open (warmed from the
    /// list, or opened before in this app run): the window starts again at
    /// the newest turns.
    func open(_ cache: Cache?) async {
        guard !opened else {
            resetWindow()
            return
        }
        if let cache {
            let key = key, now = now(), window = Self.window
            let read = await Task.detached(priority: .userInitiated) { () -> (Cache.CachedTurns, [TurnRow])? in
                guard let held = try? await cache.fabricTurns(key), !held.turns.isEmpty else { return nil }
                return (held, held.turns.suffix(window).map { TurnRow($0, now: now) })
            }.value
            // opened meanwhile by another caller (a warm-up, then the screen)
            guard !opened else { return }
            if let (held, rows) = read {
                for r in rows { built[r.id] = r }
                more = held.older
                synced = held.synced
                cachedOldest = held.turns.first?.ended
                fromCache = true
                // with whatever the stream brought meanwhile
                merge(held.turns, force: true)
            }
        }
        opened = true
    }

    /// The turns that ended since the watermark (a pull to refresh, Try
    /// again, coming back to the app). With nothing held, the newest page.
    func refresh(_ service: (any FabricService)?, cache: Cache? = nil) async {
        guard let service, !loading else { return }
        loading = true
        defer { loading = false }
        do {
            if let from = synced ?? cachedOldest {
                try await topUp(service, cache: cache, from: from)
            } else {
                let page = try await service.turns(key, before: nil, limit: 30)
                merge(page.turns)
                more = page.more
                let top = page.turns.map(\.ended).max()
                if let top { synced = max(synced ?? top, top) }
                if stillCurrent() { try? await cache?.cacheFabricTurns(page.turns, olderBeyond: page.more, syncedThrough: top) }
            }
            loadError = nil
        } catch is CancellationError {
        } catch {
            if turns.isEmpty {
                loadError = FleetStore.describe(error)
            } else {
                problem = FabricNotice(text: "Couldn't check for newer turns: \(FleetStore.describe(error))")
            }
        }
    }

    /// The turns that ended since `from`, oldest first, page by page until
    /// the server has no more. Asked from a second before it, so a turn that
    /// ended in the same second is not missed (each is kept once by id).
    /// Each page moves the watermark to its newest turn: pages read from the
    /// watermark are gap-free by construction. A server from before
    /// `after=` answers its newest page instead: that is merged as today,
    /// and if it does not reach back to the watermark, what lies between is
    /// unknown, so the feed (and the cache) start again from that page.
    private func topUp(_ service: any FabricService, cache: Cache?, from: Int) async throws {
        var after = from - 1
        var limit = 30
        for _ in 0..<50 {
            let page = try await service.turns(key, after: after, limit: limit)
            let top = page.turns.map(\.ended).max()
            guard page.isAfterPage(after: after) else {
                let reaches = page.turns.contains { $0.ended <= from }
                if !reaches {
                    // keep only what the stream brought since (newer than the page)
                    let newest = top ?? .max
                    turns = turns.filter { $0.ended > newest }
                    floor = nil
                    if stillCurrent() { try? await cache?.replaceFabricTurns(key, with: page.turns, older: page.more, syncedThrough: top) }
                } else if stillCurrent() {
                    try? await cache?.cacheFabricTurns(page.turns, syncedThrough: top)
                }
                merge(page.turns, force: true)
                if !reaches { more = page.more }
                if let top { synced = reaches ? max(synced ?? top, top) : top }
                return
            }
            merge(page.turns)
            if let top { synced = max(synced ?? top, top) }
            if stillCurrent() { try? await cache?.cacheFabricTurns(page.turns, syncedThrough: top) }
            guard page.more, let last = page.turns.last?.ended else { return }
            // never ask the same page twice (more than a page ending in one second)
            after = max(last - 1, after + 1)
            limit = 100
        }
    }

    /// There is something older to show: held in memory, or on the server.
    var hasEarlier: Bool { hiddenCount > 0 || more }

    /// Turns held but not drawn (older than the window).
    var hiddenCount: Int {
        guard let floor, let i = turns.firstIndex(where: { $0.turnID == floor }) else { return 0 }
        return i
    }

    /// "Earlier turns": the next `window` held turns at once; only when none
    /// is held, a page from the server. Asked `before` a second after the
    /// oldest held, so a turn that ended in the same second as it is not
    /// skipped (`before` is exclusive; what is held already is kept once).
    func showEarlier(_ service: (any FabricService)?, cache: Cache? = nil) async {
        if hiddenCount > 0 {
            moveFloor(back: Self.window)
            return
        }
        guard let service, more, !loadingOlder, let oldest = turns.first else { return }
        loadingOlder = true
        defer { loadingOlder = false }
        do {
            var page = try await service.turns(key, before: oldest.ended + 1, limit: 30)
            let held = Set(turns.map(\.turnID))
            if page.more, page.turns.allSatisfy({ held.contains($0.turnID) }) {
                // a whole page ended in that one second: step past it
                page = try await service.turns(key, before: oldest.ended, limit: 30)
            }
            let before = turns.count
            merge(page.turns)
            more = page.more
            // what came back is drawn up to a window's worth; the rest waits, held
            moveFloor(back: min(Self.window, turns.count - before))
            if stillCurrent() { try? await cache?.cacheFabricTurns(page.turns, olderBeyond: page.more) }
        } catch is CancellationError {
        } catch {
            problem = FabricNotice(text: "Couldn't load earlier turns: \(FleetStore.describe(error))")
        }
    }

    /// The toast has been shown.
    func clearProblem() { problem = nil }

    /// Turns from anywhere, kept once each, oldest first.
    func merge(_ new: [FabricTurn], force: Bool = false) {
        let mine = new.filter { $0.session == key }
        guard force || !mine.isEmpty else { return }
        var byID = Dictionary(turns.map { ($0.turnID, $0) }, uniquingKeysWith: { a, _ in a })
        var changed = force
        for t in mine {
            if let held = byID[t.turnID] {
                // the live event carries text a Text off route leaves out: keep the fuller one
                if held == t || held.reply.count > t.reply.count { continue }
            }
            byID[t.turnID] = t
            changed = true
        }
        guard changed else { return }
        place(Array(byID.values))
    }

    /// A long reply shown whole (its whole text parsed now, once) or collapsed again.
    func expand(_ id: String, _ on: Bool) {
        if !on {
            expanded[id] = nil
        } else if expanded[id] == nil, let t = turns.first(where: { $0.turnID == id }) {
            expanded[id] = ReplyMarkdown.parse(t.reply)
        }
    }

    // MARK: the window

    private func place(_ all: [FabricTurn]) {
        turns = all.sorted { ($0.ended, $0.started, $0.turnID) < ($1.ended, $1.started, $1.turnID) }
        if let f = floor, !turns.contains(where: { $0.turnID == f }) { floor = nil }
        if floor == nil, turns.count > Self.window { floor = turns[turns.count - Self.window].turnID }
        rebuild()
    }

    private func resetWindow() {
        let target = turns.count > Self.window ? turns[turns.count - Self.window].turnID : nil
        expanded = [:]
        guard target != floor else { return }
        floor = target
        rebuild()
    }

    private func moveFloor(back n: Int) {
        guard n > 0, !turns.isEmpty else { return }
        let i = floor.flatMap { f in turns.firstIndex { $0.turnID == f } } ?? 0
        // the oldest held, once reached, stays the floor: a later merge does not window again
        floor = turns[max(0, i - n)].turnID
        rebuild()
    }

    /// Rows for the drawn turns, reusing the row already built for a turn
    /// whose words and day are the same; the "N new" divider found once
    /// per change, not once per row.
    private func rebuild() {
        let start = floor.flatMap { f in turns.firstIndex { $0.turnID == f } } ?? 0
        let drawn = turns[start...]
        let now = now()
        var next: [TurnRow] = []
        next.reserveCapacity(drawn.count)
        for t in drawn {
            if let row = built[t.turnID], row.shows(t, now: now) {
                next.append(row)
            } else {
                let row = TurnRow(t, now: now)
                built[t.turnID] = row
                next.append(row)
            }
        }
        if built.count > next.count + Self.window * 2 {
            let keep = Set(next.map(\.id))
            built = built.filter { keep.contains($0.key) }
        }
        if next != rows { rows = next }
        let first = seenUpTo.flatMap { seen in drawn.first { $0.ended > seen } }?.turnID
        if firstNew != first { firstNew = first }
        let count = seenUpTo.map { seen in turns.count { $0.ended > seen } } ?? 0
        if newCount != count { newCount = count }
    }

    /// The person has looked: remember the newest turn.
    func looked() {
        if let last = turns.last { LastLooked.shared.look(key, upTo: last.ended) }
    }
}

/// Actions in flight on a screen, by name: a second tap on a button whose
/// first is still on its way does nothing (an order posted twice is two
/// orders). Each button is disabled while its own name runs.
@MainActor @Observable
final class InFlight {
    private(set) var running: Set<String> = []

    func isRunning(_ name: String) -> Bool { running.contains(name) }

    /// Run `change` unless `name` is already running: its answer (nil:
    /// done), or nil-wrapped when it was not run at all.
    @discardableResult
    func run(_ name: String, _ change: @MainActor () async -> String?) async -> String?? {
        guard !running.contains(name) else { return .none }
        running.insert(name)
        defer { running.remove(name) }
        return .some(await change())
    }
}
