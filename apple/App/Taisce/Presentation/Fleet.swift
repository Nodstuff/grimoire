import Foundation
import TaisceKit

// The Agents tab's view models (Agent Fabric, Slice 1: Watch). Pure: the
// screen draws what these say.

/// How long a snooze lasts (the design: "for an hour or until tomorrow").
/// "Tomorrow" is the next morning at 09:00 on this device's clock (the hour
/// an all-day to-do alerts at). In the small hours, before 05:00, the next
/// morning is today's: someone up at 01:00 means the morning that is
/// coming, not the one 33 hours away, and the label says which.
enum FleetSnooze: CaseIterable, Hashable, Sendable {
    case oneHour, nextMorning

    /// Before this hour, "the morning" is still today's.
    static let smallHours = 5

    private func isSmallHours(_ now: Date, _ calendar: Calendar) -> Bool {
        calendar.component(.hour, from: now) < Self.smallHours
    }

    func title(now: Date = .now, calendar: Calendar = .current) -> String {
        switch self {
        case .oneHour: "For an hour"
        case .nextMorning: isSmallHours(now, calendar) ? "Until this morning" : "Until tomorrow"
        }
    }

    /// When the snooze ends.
    func until(now: Date = .now, calendar: Calendar = .current) -> Date {
        switch self {
        case .oneHour:
            return now.addingTimeInterval(3600)
        case .nextMorning:
            let day = isSmallHours(now, calendar) ? now : calendar.date(byAdding: .day, value: 1, to: now)
            let morning = day.flatMap { calendar.date(bySettingHour: Due.defaultAlertHour, minute: 0, second: 0, of: $0) }
            return morning ?? now.addingTimeInterval(86400)
        }
    }
}

extension SessionState {
    /// The state in words (the notification's own for the three it announces).
    var words: String {
        switch self {
        case .needsYou: "Needs you"
        case .stalled: "Stalled"
        case .finished: "Finished"
        case .waitingOnPeer: "Waiting on a peer"
        case .working: "Working"
        case .unknown: "Unknown state"
        case .idle: "Idle"
        case .asleep: "Asleep"
        case .exited: "Exited"
        }
    }

    var tone: LapisTone {
        switch self {
        case .needsYou: .yellow
        case .stalled: .red
        case .finished: .green
        case .waitingOnPeer: .agent
        case .working: .accent
        case .unknown, .idle, .asleep, .exited: .neutral
        }
    }

    var symbol: String {
        switch self {
        case .needsYou: "questionmark.bubble"
        case .stalled: "exclamationmark.triangle"
        case .finished: "checkmark.circle"
        case .waitingOnPeer: "arrow.left.arrow.right"
        case .working: "circle.dotted"
        case .unknown: "questionmark.circle"
        case .idle: "pause.circle"
        case .asleep: "moon"
        case .exited: "xmark.circle"
        }
    }
}

/// Times as the fleet says them.
enum FleetTime {
    /// How long something has lasted: "now", "5 min", "2 h", "3 d".
    static func elapsed(since: Int, now: Date) -> String {
        let s = max(0, Int(now.timeIntervalSince1970) - since)
        switch s {
        case ..<60: return "now"
        case ..<3600: return "\(s / 60) min"
        case ..<86400: return "\(s / 3600) h"
        default: return "\(s / 86400) d"
        }
    }

    /// A moment: "12:41" today, "Thu 12:41" within a week either way, else
    /// "3 Nov"; the hour as the locale writes it ("12:41 PM" where the
    /// clock is a 12-hour one).
    static func clock(_ unix: Int, now: Date, calendar: Calendar = .current, locale: Locale = .current) -> String {
        let date = Date(timeIntervalSince1970: TimeInterval(unix))
        var style = Date.FormatStyle(locale: locale, calendar: calendar, timeZone: calendar.timeZone)
        if calendar.isDate(date, inSameDayAs: now) {
            style = style.hour().minute()
        } else if abs(date.timeIntervalSince(now)) < 7 * 86400 {
            style = style.weekday(.abbreviated).hour().minute()
        } else {
            style = style.day().month(.abbreviated)
        }
        return date.formatted(style)
    }
}

/// One session's row.
struct FleetRow: Identifiable, Hashable {
    /// A context window this full is "nearly full".
    static let contextWarnAt = 85

    var session: FleetSession
    /// its runtime's stream is down: this is the last known state
    var offline: Bool
    /// "Working · Bash · 5 min"; of an offline runtime's, "Working · Bash · last known"
    var status: String
    /// the question, the last line, or the note
    var detail: String?
    /// "personal · agent-fabric · context 41%"
    var meta: String?
    var contextNearlyFull: Bool
    /// "Snoozed until 09:00"
    var snoozed: String?
    /// what waits on you from it: "Wants approval · Release and deploy scripts"
    var waiting: String?
    /// the open item behind `waiting` (an approval opens its sheet)
    var waitingItem: FabricAttentionItem?
    /// prompts queued for it
    var queued: Int

    var id: FleetSessionKey { session.key }
    var tone: LapisTone { offline ? .neutral : session.state.tone }

    init(_ s: FleetSession, offline: Bool, now: Date, calendar: Calendar = .current, locale: Locale = .current, attention: [FabricAttentionItem] = []) {
        session = s
        self.offline = offline
        queued = s.queue.count
        var parts = [s.state.words]
        if s.state == .working, !s.tool.isEmpty { parts.append(s.tool) }
        // how long it has been so; of a Mac that is away, only what was last known
        parts.append(offline ? "last known" : FleetTime.elapsed(since: s.since, now: now))
        status = parts.joined(separator: " \u{00B7} ")
        // what it is waiting on or ended with is its last line; in a turn,
        // its note says what it is doing
        let said = [s.lastLine, s.note].first { !$0.isEmpty }
        let doing = [s.note, s.lastLine].first { !$0.isEmpty }
        switch s.state {
        case .needsYou, .stalled, .finished: detail = said
        default: detail = doing
        }
        var m: [String] = []
        if !s.account.isEmpty { m.append(s.account) }
        if !s.branch.isEmpty { m.append(s.branch) }
        if let pct = s.contextUsedPct { m.append("context \(pct)%") }
        meta = m.isEmpty ? nil : m.joined(separator: " \u{00B7} ")
        contextNearlyFull = (s.contextUsedPct ?? 0) >= Self.contextWarnAt
        snoozed = s.isSnoozed(at: now) ? s.quietUntil.map { "Snoozed until \(FleetTime.clock($0, now: now, calendar: calendar, locale: locale))" } : nil
        // the runtime's items first (an approval outranks a question), newest first
        let item = attention.first { $0.kind == .approval } ?? attention.first { $0.kind == .question } ?? attention.first
        waitingItem = item
        if let item {
            switch item.kind {
            case .approval: waiting = "Wants approval" + (item.guardrailLabel.isEmpty ? "" : " \u{00B7} \(item.guardrailLabel)")
            case .question: waiting = "Asks you"
            case .dialog: waiting = "A dialog at the Mac"
            case .tamper: waiting = "Held: hooks switched off"
            case .wakeOffer: waiting = "Mail waiting \u{00B7} wake it?"
            case .askWaiting: waiting = "Asked while asleep"
            case .pairPaused: waiting = "Paused talking"
            case .unknown: waiting = nil
            }
            if !item.text.isEmpty { detail = item.text }
        }
    }

    /// What VoiceOver reads for the row.
    var spoken: String {
        [session.address, status, waiting, detail, meta, snoozed, queued > 0 ? "\(queued) queued" : nil].compactMap { $0 }.joined(separator: ", ")
    }
}

/// A runtime's line at the top of the fleet: "Mac awake", "Mac awake · no
/// sessions" (its runtime closed its stream cleanly), or "Mac asleep since
/// 12:41" (the design's words: a runtime whose stream dropped is a Mac that
/// is asleep, shut or off the network; its sessions are shown as last known).
struct FleetRuntimeNote: Identifiable, Hashable {
    var id: String
    var name: String
    var online: Bool
    /// online or idle: the Mac is awake either way
    var awake: Bool
    var title: String
    /// "mbp · its sessions are shown as last known"
    var detail: String

    init(_ r: FleetRuntime, now: Date, calendar: Calendar = .current, locale: Locale = .current) {
        id = r.id
        name = r.name
        online = r.online
        awake = r.online || r.connection == .idle
        if r.online {
            title = "Mac awake"
            detail = r.name
        } else if r.connection == .idle {
            // its runtime closed cleanly: awake, nothing to report. Never "asleep".
            title = "Mac awake \u{00B7} no sessions"
            detail = r.name
        } else if let seen = r.lastSeenAt {
            title = "Mac asleep since \(FleetTime.clock(seen, now: now, calendar: calendar, locale: locale))"
            detail = "\(r.name) \u{00B7} sessions shown as last known"
        } else {
            title = "Mac not connected yet"
            detail = "\(r.name) \u{00B7} enrolled, never seen"
        }
    }
}

/// One Claude account's headroom: "personal", "session 62% left · resets
/// 14:05", "week 39% left".
struct FleetHeadroom: Identifiable, Hashable {
    /// An account this used is "nearly spent".
    static let warnAtUsed = 90

    var id: String { name }
    var name: String
    /// "session 62% left · resets 14:05"; nil = not reported
    var session: String?
    var week: String?
    /// nearly spent, with sessions running on it
    var warning: Bool

    init(_ a: FleetAccount, running: Bool, now: Date, calendar: Calendar = .current, locale: Locale = .current) {
        name = a.name
        func line(_ label: String, used: Int?, resets: Int?) -> String? {
            guard let used else { return nil }
            let left = "\(label) \(max(0, 100 - used))% left"
            // a reset already past says nothing
            guard let resets, Double(resets) > now.timeIntervalSince1970 else { return left }
            return "\(left) \u{00B7} resets \(FleetTime.clock(resets, now: now, calendar: calendar, locale: locale))"
        }
        session = line("session", used: a.sessionUsedPct, resets: a.sessionResetsAt)
        week = line("week", used: a.weekUsedPct, resets: a.weekResetsAt)
        warning = running && max(a.sessionUsedPct ?? 0, a.weekUsedPct ?? 0) >= Self.warnAtUsed
    }

    var spoken: String {
        ([name] + [session, week].compactMap { $0 } + (warning ? ["nearly spent"] : [])).joined(separator: ", ")
    }
}

/// Today's way in to Agents on a phone: "Agents", "1 needs you · 2 working".
struct FleetCard: Hashable {
    var subtitle: String
    /// a session is waiting on its person or stuck (and not snoozed): the
    /// card then goes first on Today; otherwise it sits after the pins
    var attention: Bool

    /// Nil when there is nothing to show: no fleet here, or no runtime enrolled.
    init?(_ fleet: Fleet, available: Bool, now: Date = .now) {
        guard available, !fleet.runtimes.isEmpty else { return nil }
        let board = FleetBoard(fleet, now: now)
        let count = fleet.sessions.count
        let asleep = fleet.runtimes.allSatisfy { $0.connection == .offline }
        subtitle = board.summary ?? (asleep ? "Mac asleep" : count == 0 ? "No sessions" : count == 1 ? "1 session, none needs you" : "\(count) sessions, none needs you")
        attention = fleet.attentionCount(at: now) > 0
    }
}

/// The Agents screen's content, from the fleet as held. Pinned sessions
/// come first, whatever their state. The rest keep the order they are held
/// in (the server's, by how much each needs its person: needs you, stalled,
/// finished, waiting on a peer, working); the ones doing nothing (idle,
/// asleep, exited) fold away under a count, except one idle for under an
/// hour: between the turns of a conversation a session is idle, and it
/// should not drop out of sight mid-chat (Tom, 2026-11-21).
struct FleetBoard: Hashable {
    /// An idle session this recent stays among the active ones.
    static let recent: TimeInterval = 3600

    var runtimes: [FleetRuntimeNote] = []
    var headroom: [FleetHeadroom] = []
    /// the sessions this phone pinned, in the server's order
    var pinned: [FleetRow] = []
    var rows: [FleetRow] = []
    var resting: [FleetRow] = []
    /// the fabrics sessions are in, for the filter (named ones and projects' own)
    var fabrics: [String] = []
    /// a Mac has Hold all on
    var held = false

    /// no runtime is enrolled
    var hasNoRuntime: Bool { runtimes.isEmpty }
    var hasNoSessions: Bool { pinned.isEmpty && rows.isEmpty && resting.isEmpty }

    init() {}

    init(_ fleet: Fleet, now: Date, pins: Set<FleetSessionKey> = [], calendar: Calendar = .current, locale: Locale = .current) {
        runtimes = fleet.runtimes.map { FleetRuntimeNote($0, now: now, calendar: calendar, locale: locale) }
        let online = Set(fleet.runtimes.filter(\.online).map(\.id))
        let all = fleet.sessions.map { FleetRow($0, offline: !online.contains($0.runtimeID), now: now, calendar: calendar, locale: locale, attention: fleet.attention(for: $0.key)) }
        var seen = Set<String>()
        fabrics = fleet.sessions.map { $0.fabric.isEmpty ? $0.project : $0.fabric }.filter { !$0.isEmpty && seen.insert($0).inserted }
        held = fleet.runtimes.contains(where: \.held)
        let nowS = Int(now.timeIntervalSince1970)
        let atRest: (FleetRow) -> Bool = { row in
            row.session.state.isResting && !(row.session.state == .idle && nowS - row.session.since < Int(Self.recent))
        }
        pinned = all.filter { pins.contains($0.id) }
        rows = all.filter { !pins.contains($0.id) && !atRest($0) }
        resting = all.filter { !pins.contains($0.id) && atRest($0) }
        // an account "has running sessions" while one of an online runtime's is not at rest
        let running = Set(fleet.sessions.filter { !$0.state.isResting && online.contains($0.runtimeID) }.map(\.account))
        // one line per account name: two Macs on one account report the same
        // allowance, and the later report is the truer
        var latest: [String: (account: FleetAccount, at: Int, order: Int)] = [:]
        for (i, r) in fleet.runtimes.enumerated() {
            for (j, a) in r.accounts.enumerated() {
                let at = r.accountsAt ?? 0
                if let held = latest[a.name], held.at >= at { continue }
                latest[a.name] = (a, at, latest[a.name]?.order ?? i * 1000 + j)
            }
        }
        headroom = latest.values.sorted { $0.order < $1.order }
            .map { FleetHeadroom($0.account, running: running.contains($0.account.name), now: now, calendar: calendar, locale: locale) }
    }

    /// Only the sessions in `fabric` (nil: all).
    func only(_ fabric: String?) -> FleetBoard {
        guard let fabric else { return self }
        var b = self
        let keep: (FleetRow) -> Bool = { ($0.session.fabric.isEmpty ? $0.session.project : $0.session.fabric) == fabric }
        b.pinned = pinned.filter(keep)
        b.rows = rows.filter(keep)
        b.resting = resting.filter(keep)
        return b
    }

    /// "2 need you · 3 working", for the header. A snoozed session is
    /// listed as ever and not counted as needing you.
    var summary: String? {
        let live = rows.filter { !$0.offline }
        let needs = live.count { $0.session.state.wantsAttention && $0.snoozed == nil }
        let working = live.count { $0.session.state == .working }
        var parts: [String] = []
        if needs > 0 { parts.append(needs == 1 ? "1 needs you" : "\(needs) need you") }
        if working > 0 { parts.append("\(working) working") }
        return parts.isEmpty ? nil : parts.joined(separator: " \u{00B7} ")
    }
}

/// The sessions this phone keeps pinned at the top of Agents. Per device,
/// in its defaults: a pin is how this person reads the list, not something
/// about the fleet. One "runtime\tsession" per line.
enum FleetPins {
    static let key = "agents.pinned"

    static func decode(_ raw: String) -> Set<FleetSessionKey> {
        Set(raw.split(separator: "\n").compactMap { line in
            let parts = line.split(separator: "\t", omittingEmptySubsequences: false)
            guard parts.count == 2, !parts[0].isEmpty, !parts[1].isEmpty else { return nil }
            return FleetSessionKey(runtimeID: String(parts[0]), id: String(parts[1]))
        })
    }

    static func encode(_ pins: Set<FleetSessionKey>) -> String {
        pins.map { "\($0.runtimeID)\t\($0.id)" }.sorted().joined(separator: "\n")
    }

    /// `raw` with `key` pinned or not.
    static func set(_ key: FleetSessionKey, pinned: Bool, in raw: String) -> String {
        var pins = decode(raw)
        if pinned { pins.insert(key) } else { pins.remove(key) }
        return encode(pins)
    }
}
