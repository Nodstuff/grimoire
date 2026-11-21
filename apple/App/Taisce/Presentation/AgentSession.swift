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

/// One turn as the feed shows it. Its reply's Markdown is parsed here, once
/// per turn (the feed keeps its rows), never in a view's `body`: a long
/// session re-parsed every reply on every redraw.
struct TurnRow: Identifiable, Hashable {
    /// a reply longer than this shows its first this-many characters until "Show all"
    static let collapsedLength = 900

    var id: String
    var prompt: String
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
    /// the whole reply, as inline Markdown
    let replyText: AttributedString
    /// its first `collapsedLength` characters and an ellipsis; nil when it is not that long
    let replyPreview: AttributedString?

    init(_ t: FabricTurn, now: Date) {
        id = t.turnID
        prompt = t.prompt
        reply = t.reply
        var a: [String] = []
        if t.toolCount > 0 { a.append(t.toolCount == 1 ? "1 tool" : "\(t.toolCount) tools") }
        if let o = FabricWords.outcome(t.outcome) { a.append(o) }
        activity = a.isEmpty ? nil : a.joined(separator: " \u{00B7} ")
        source = FabricWords.source(t.source)
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
        replyText = t.reply.isEmpty ? AttributedString() : Self.markdown(t.reply)
        replyPreview = t.reply.count > Self.collapsedLength ? Self.markdown(String(t.reply.prefix(Self.collapsedLength)) + "\u{2026}") : nil
    }

    /// Inline Markdown (bold, code, links), as Claude writes it.
    static func markdown(_ s: String) -> AttributedString {
        (try? AttributedString(markdown: s, options: .init(interpretedSyntax: .inlineOnlyPreservingWhitespace))) ?? AttributedString(s)
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

/// The feed of one session's turns: the newest page from the server,
/// older pages on demand, and the stream's new turns as they finish.
@MainActor @Observable
final class SessionFeed {
    let key: FleetSessionKey
    /// oldest first, as read
    private(set) var turns: [FabricTurn] = [] {
        didSet { rows = Self.rows(for: turns, reusing: rows, now: now()) }
    }
    /// `turns` as the feed shows them, each built (its Markdown parsed) once
    /// and kept while its turn and its day are unchanged
    private(set) var rows: [TurnRow] = []
    /// the clock rows are worded against (a test's fixed one)
    @ObservationIgnored var now: () -> Date = Date.init
    private(set) var more = false
    private(set) var loading = false
    private(set) var loadError: String?
    /// the newest turn's end the person had seen when the screen opened
    private(set) var seenUpTo: Int?

    init(key: FleetSessionKey) {
        self.key = key
        seenUpTo = LastLooked.shared.seen(key)
    }

    func load(_ service: (any FabricService)?) async {
        guard let service else { return }
        loading = true
        defer { loading = false }
        do {
            let page = try await service.turns(key, before: nil, limit: 30)
            merge(page.turns)
            more = page.more
            loadError = nil
        } catch {
            if turns.isEmpty { loadError = FleetStore.describe(error) }
        }
    }

    func loadOlder(_ service: (any FabricService)?) async {
        guard let service, more, !loading, let oldest = turns.first else { return }
        loading = true
        defer { loading = false }
        if let page = try? await service.turns(key, before: oldest.ended, limit: 30) {
            merge(page.turns)
            more = page.more
        }
    }

    /// Turns from anywhere, kept once each, oldest first.
    func merge(_ new: [FabricTurn]) {
        var byID = Dictionary(turns.map { ($0.turnID, $0) }, uniquingKeysWith: { a, _ in a })
        for t in new where t.session == key {
            // the live event carries text a Text off route leaves out: keep the fuller one
            if let held = byID[t.turnID], held.reply.count > t.reply.count { continue }
            byID[t.turnID] = t
        }
        turns = byID.values.sorted { ($0.ended, $0.started) < ($1.ended, $1.started) }
    }

    /// A row per turn, reusing the row already built for a turn whose words
    /// and day are the same (a reply only grows, from the route's to the
    /// live event's: its length says whether it changed).
    static func rows(for turns: [FabricTurn], reusing old: [TurnRow], now: Date) -> [TurnRow] {
        let held = Dictionary(old.map { ($0.id, $0) }, uniquingKeysWith: { a, _ in a })
        return turns.map { t in
            if let row = held[t.turnID], row.reply.utf8.count == t.reply.utf8.count, row.prompt.utf8.count == t.prompt.utf8.count,
               row.textOff == (t.textOff && t.prompt.isEmpty && t.reply.isEmpty),
               row.when == FleetTime.clock(t.ended, now: now) {
                return row
            }
            return TurnRow(t, now: now)
        }
    }

    /// Turns that finished after the person last looked.
    var newCount: Int {
        guard let seenUpTo else { return 0 }
        return turns.count { $0.ended > seenUpTo }
    }

    /// The first turn the person has not seen (where "N new" sits).
    var firstNew: String? {
        guard let seenUpTo else { return nil }
        return turns.first { $0.ended > seenUpTo }?.turnID
    }

    /// The person has looked: remember the newest turn.
    func looked() {
        if let last = turns.last { LastLooked.shared.look(key, upTo: last.ended) }
    }
}
