import Foundation
import TaisceKit

// The Agents tab's view models (Agent Fabric, Slice 1: Watch). Pure: the
// screen draws what these say.

/// How long a snooze lasts (the design: "for an hour or until tomorrow").
enum FleetSnooze: CaseIterable, Hashable, Sendable {
    case oneHour, tomorrowMorning

    var title: String {
        switch self {
        case .oneHour: "For an hour"
        case .tomorrowMorning: "Until tomorrow"
        }
    }

    /// When the snooze ends: an hour on, or 09:00 tomorrow on this device's
    /// clock (the hour an all-day to-do alerts at).
    func until(now: Date = .now, calendar: Calendar = .current) -> Date {
        switch self {
        case .oneHour:
            return now.addingTimeInterval(3600)
        case .tomorrowMorning:
            let tomorrow = calendar.date(byAdding: .day, value: 1, to: now)
            let morning = tomorrow.flatMap { calendar.date(bySettingHour: Due.defaultAlertHour, minute: 0, second: 0, of: $0) }
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

    /// A moment: "12:41" today, "Thu 12:41" within a week either way, else "3 Nov".
    static func clock(_ unix: Int, now: Date, calendar: Calendar = .current, locale: Locale = .current) -> String {
        let date = Date(timeIntervalSince1970: TimeInterval(unix))
        var style = Date.FormatStyle(locale: locale, calendar: calendar, timeZone: calendar.timeZone)
        if calendar.isDate(date, inSameDayAs: now) {
            style = style.hour(.twoDigits(amPM: .omitted)).minute(.twoDigits)
        } else if abs(date.timeIntervalSince(now)) < 7 * 86400 {
            style = style.weekday(.abbreviated).hour(.twoDigits(amPM: .omitted)).minute(.twoDigits)
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

    var id: FleetSessionKey { session.key }
    var tone: LapisTone { offline ? .neutral : session.state.tone }

    init(_ s: FleetSession, offline: Bool, now: Date, calendar: Calendar = .current, locale: Locale = .current) {
        session = s
        self.offline = offline
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
    }

    /// What VoiceOver reads for the row.
    var spoken: String {
        [session.address, status, detail, meta, snoozed].compactMap { $0 }.joined(separator: ", ")
    }
}

/// A runtime's line at the top of the fleet: "Mac awake", or "Mac asleep
/// since 12:41" (the design's words: a runtime whose stream is down is a
/// Mac that is asleep, shut or off the network; its sessions are shown as
/// last known).
struct FleetRuntimeNote: Identifiable, Hashable {
    var id: String
    var name: String
    var online: Bool
    var title: String
    /// "mbp · its sessions are shown as last known"
    var detail: String

    init(_ r: FleetRuntime, now: Date, calendar: Calendar = .current, locale: Locale = .current) {
        id = r.id
        name = r.name
        online = r.online
        if r.online {
            title = "Mac awake"
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

/// Today's way in to Agents: "Agents", "1 needs you · 2 working".
struct FleetCard: Hashable {
    var subtitle: String
    /// a session is waiting on its person or stuck
    var attention: Bool

    /// Nil when there is nothing to show: no fleet here, or no runtime enrolled.
    init?(_ fleet: Fleet, available: Bool, now: Date = .now) {
        guard available, !fleet.runtimes.isEmpty else { return nil }
        let board = FleetBoard(fleet, now: now)
        let count = fleet.sessions.count
        let asleep = fleet.runtimes.allSatisfy { !$0.online }
        subtitle = board.summary ?? (asleep ? "Mac asleep" : count == 0 ? "No sessions" : count == 1 ? "1 session, none needs you" : "\(count) sessions, none needs you")
        attention = fleet.attentionCount > 0
    }
}

/// The Agents screen's content, from the fleet as held. The sessions keep
/// the order they are held in (the server's, by how much each needs its
/// person: needs you, stalled, finished, waiting on a peer, working); the
/// ones doing nothing (idle, asleep, exited) fold away under a count.
struct FleetBoard: Hashable {
    var runtimes: [FleetRuntimeNote] = []
    var headroom: [FleetHeadroom] = []
    var rows: [FleetRow] = []
    var resting: [FleetRow] = []

    /// no runtime is enrolled
    var hasNoRuntime: Bool { runtimes.isEmpty }
    var hasNoSessions: Bool { rows.isEmpty && resting.isEmpty }

    init() {}

    init(_ fleet: Fleet, now: Date, calendar: Calendar = .current, locale: Locale = .current) {
        runtimes = fleet.runtimes.map { FleetRuntimeNote($0, now: now, calendar: calendar, locale: locale) }
        let online = Set(fleet.runtimes.filter(\.online).map(\.id))
        let all = fleet.sessions.map { FleetRow($0, offline: !online.contains($0.runtimeID), now: now, calendar: calendar, locale: locale) }
        rows = all.filter { !$0.session.state.isResting }
        resting = all.filter { $0.session.state.isResting }
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

    /// "2 need you · 3 working", for the header.
    var summary: String? {
        let live = rows.filter { !$0.offline }
        let needs = live.count { $0.session.state.wantsAttention }
        let working = live.count { $0.session.state == .working }
        var parts: [String] = []
        if needs > 0 { parts.append(needs == 1 ? "1 needs you" : "\(needs) need you") }
        if working > 0 { parts.append("\(working) working") }
        return parts.isEmpty ? nil : parts.joined(separator: " \u{00B7} ")
    }
}
