import Foundation

// Agent Fabric, Slice 1 (Watch): the person's fleet as the server shows it
// (`crates/daemon/src/fabric/view.rs`, ADR 0007). Times are unix seconds.

/// What a session is doing, as its runtime reports it
/// (`proto/taisce/runtime/v1/runtime.proto`). `unknown` is a state this
/// build has no name for (a newer server's or runtime's): it is shown, and
/// sorted by the server beside `working`.
public enum SessionState: String, Sendable, Hashable, CaseIterable, Codable {
    /// stopped on a question, an approval or a dialog
    case needsYou = "needs_you"
    /// working, with no sign of life for five minutes
    case stalled
    /// a turn ended and its person has not looked yet
    case finished
    /// asked another agent something and is waiting
    case waitingOnPeer = "waiting_on_peer"
    /// in a turn
    case working
    case unknown
    case idle
    /// its process is not running (a pane restored and not yet shown)
    case asleep
    case exited

    public init(from decoder: any Decoder) throws {
        self = SessionState(rawValue: try decoder.singleValueContainer().decode(String.self)) ?? .unknown
    }

    /// Nothing is happening (idle, asleep, exited): the fleet folds these
    /// away under a count.
    public var isResting: Bool {
        switch self {
        case .idle, .asleep, .exited: true
        default: false
        }
    }

    /// The session is waiting on its person or stuck: what the Agents badge counts.
    public var wantsAttention: Bool { self == .needsYou || self == .stalled }
}

/// A session's identity: its id is its runtime's own (a terminal pane's
/// UUID), so two runtimes may hold the same one.
public struct FleetSessionKey: Sendable, Hashable, Codable {
    public var runtimeID: String
    public var id: String

    public init(runtimeID: String, id: String) {
        self.runtimeID = runtimeID
        self.id = id
    }
}

/// One Claude session, as its runtime last reported it.
public struct FleetSession: Sendable, Hashable, Codable, Identifiable {
    /// the runtime's own id for it
    public var sessionID: String
    public var runtimeID: String
    /// `project/worktree`, e.g. `taisce/main`
    public var address: String
    public var fabric: String
    public var project: String
    public var worktree: String
    public var branch: String
    /// the Claude account it runs on ("" = not reported)
    public var account: String
    public var state: SessionState
    /// the tool it is running (while working; else "")
    public var tool: String
    /// its one-line status ("" = none)
    public var note: String
    /// the last line it said ("" = none)
    public var lastLine: String
    /// how full its context window is, 0–100; nil = unknown
    public var contextUsedPct: Int?
    /// when it entered `state`, by the runtime's clock
    public var since: Int
    /// when the server last heard it exists
    public var updatedAt: Int
    /// snoozed until then: the server sends no notification for it before
    /// that. nil = not snoozed (or a server without snooze)
    public var quietUntil: Int?

    public var key: FleetSessionKey { FleetSessionKey(runtimeID: runtimeID, id: sessionID) }
    public var id: FleetSessionKey { key }

    public init(
        sessionID: String, runtimeID: String, address: String, fabric: String = "", project: String = "", worktree: String = "",
        branch: String = "", account: String = "", state: SessionState, tool: String = "", note: String = "", lastLine: String = "",
        contextUsedPct: Int? = nil, since: Int, updatedAt: Int, quietUntil: Int? = nil
    ) {
        self.sessionID = sessionID
        self.runtimeID = runtimeID
        self.address = address
        self.fabric = fabric
        self.project = project
        self.worktree = worktree
        self.branch = branch
        self.account = account
        self.state = state
        self.tool = tool
        self.note = note
        self.lastLine = lastLine
        self.contextUsedPct = contextUsedPct
        self.since = since
        self.updatedAt = updatedAt
        self.quietUntil = quietUntil
    }

    enum CodingKeys: String, CodingKey {
        case sessionID = "id"
        case runtimeID = "runtime_id"
        case address, fabric, project, worktree, branch, account, state, tool, note
        case lastLine = "last_line"
        case contextUsedPct = "context_used_pct"
        case since
        case updatedAt = "updated_at"
        case quietUntil = "quiet_until"
    }

    /// Snoozed at `now`: a `quietUntil` that has passed is no snooze (the
    /// server lifts the mark a few seconds after its time).
    public func isSnoozed(at now: Date = .now) -> Bool {
        quietUntil.map { Double($0) > now.timeIntervalSince1970 } ?? false
    }
}

/// One Claude account's headroom, as its runtime last reported it. Any of
/// the four may be unknown.
public struct FleetAccount: Sendable, Hashable, Codable {
    public var name: String
    /// percent of the current session window used, 0–100
    public var sessionUsedPct: Int?
    public var sessionResetsAt: Int?
    /// percent of the week's allowance used, 0–100
    public var weekUsedPct: Int?
    public var weekResetsAt: Int?

    public init(name: String, sessionUsedPct: Int? = nil, sessionResetsAt: Int? = nil, weekUsedPct: Int? = nil, weekResetsAt: Int? = nil) {
        self.name = name
        self.sessionUsedPct = sessionUsedPct
        self.sessionResetsAt = sessionResetsAt
        self.weekUsedPct = weekUsedPct
        self.weekResetsAt = weekResetsAt
    }

    enum CodingKeys: String, CodingKey {
        case name
        case sessionUsedPct = "session_used_pct"
        case sessionResetsAt = "session_resets_at"
        case weekUsedPct = "week_used_pct"
        case weekResetsAt = "week_resets_at"
    }
}

/// An enrolled runtime: a program hosting Claude sessions on a machine
/// (Workbox on a Mac).
public struct FleetRuntime: Sendable, Hashable, Codable, Identifiable {
    public var id: String
    public var name: String
    public var createdAt: Int
    /// its stream to the server is up right now
    public var online: Bool
    /// when its stream was last up (now, while it is); nil = it has never connected
    public var lastSeenAt: Int?
    public var version: String
    public var accounts: [FleetAccount]
    public var accountsAt: Int?

    public init(id: String, name: String, createdAt: Int = 0, online: Bool, lastSeenAt: Int? = nil, version: String = "", accounts: [FleetAccount] = [], accountsAt: Int? = nil) {
        self.id = id
        self.name = name
        self.createdAt = createdAt
        self.online = online
        self.lastSeenAt = lastSeenAt
        self.version = version
        self.accounts = accounts
        self.accountsAt = accountsAt
    }

    enum CodingKeys: String, CodingKey {
        case id, name, online, version, accounts
        case createdAt = "created_at"
        case lastSeenAt = "last_seen_at"
        case accountsAt = "accounts_at"
    }
}

/// One person's fleet (`GET /api/fabric/sessions`, and the stream's `fleet`
/// event): runtimes oldest first, sessions in the server's attention order.
public struct Fleet: Sendable, Hashable, Codable {
    /// the server's counter for this fleet; it starts again when the daemon does
    public var rev: Int
    public var runtimes: [FleetRuntime]
    public var sessions: [FleetSession]

    public init(rev: Int = 0, runtimes: [FleetRuntime] = [], sessions: [FleetSession] = []) {
        self.rev = rev
        self.runtimes = runtimes
        self.sessions = sessions
    }

    public func runtime(_ id: String) -> FleetRuntime? { runtimes.first { $0.id == id } }

    /// Sessions waiting on their person or stuck at `now`: on a runtime
    /// that is online (an offline runtime's are only what was last known),
    /// and not snoozed (a snooze says "I know; stop telling me").
    public func attentionCount(at now: Date = .now) -> Int {
        let online = Set(runtimes.filter(\.online).map(\.id))
        return sessions.count { $0.state.wantsAttention && online.contains($0.runtimeID) && !$0.isSnoozed(at: now) }
    }
}

/// One change to a fleet, as the stream sends it.
public enum FleetChange: Sendable, Hashable {
    /// the whole fleet (a stream's first event)
    case fleet(Fleet)
    /// a session, whole: new or changed
    case session(FleetSession)
    case sessionGone(FleetSessionKey)
    /// a runtime, whole: new or changed
    case runtime(FleetRuntime)
    /// the runtime was removed, and every session of it with it
    case runtimeGone(id: String)
}

extension Fleet {
    /// Apply a change the stream sent with `rev` (the fleet's counter after
    /// it). False when it is older than what is held and was ignored.
    ///
    /// Sessions keep their place: a changed session is replaced where it
    /// stands and a new one goes last, so a row never moves under a thumb.
    /// The server's order comes back with the next whole fleet (a refresh,
    /// or a new connection).
    @discardableResult
    public mutating func apply(_ change: FleetChange, rev: Int) -> Bool {
        if case .fleet(let fleet) = change {
            self = fleet
            return true
        }
        guard rev > self.rev else { return false }
        self.rev = rev
        switch change {
        case .fleet:
            break
        case .session(let s):
            if let i = sessions.firstIndex(where: { $0.key == s.key }) { sessions[i] = s } else { sessions.append(s) }
        case .sessionGone(let key):
            sessions.removeAll { $0.key == key }
        case .runtime(let r):
            if let i = runtimes.firstIndex(where: { $0.id == r.id }) { runtimes[i] = r } else { runtimes.append(r) }
        case .runtimeGone(let id):
            runtimes.removeAll { $0.id == id }
            sessions.removeAll { $0.runtimeID == id }
        }
        return true
    }

    /// Take a fleet read on its own (`GET /api/fabric/sessions`: a pull to
    /// refresh, or coming back to the screen), unless the stream is already
    /// past it.
    @discardableResult
    public mutating func refresh(with fleet: Fleet) -> Bool {
        guard fleet.rev >= rev else { return false }
        self = fleet
        return true
    }
}

extension FleetChange {
    /// A stream event as a change and the fleet's `rev` after it (the
    /// event's `id:`). Nil for an event this build has no name for (a newer
    /// server's); throws when a known event's data cannot be read.
    public static func decode(_ event: SSEEvent) throws -> (change: FleetChange, rev: Int)? {
        let data = Data(event.data.utf8)
        let decoder = JSONDecoder()
        struct Gone: Decodable {
            var id: String
            var runtime_id: String?
        }
        let change: FleetChange
        switch event.event {
        case "fleet": change = .fleet(try decoder.decode(Fleet.self, from: data))
        case "session": change = .session(try decoder.decode(FleetSession.self, from: data))
        case "runtime": change = .runtime(try decoder.decode(FleetRuntime.self, from: data))
        case "session_gone":
            let gone = try decoder.decode(Gone.self, from: data)
            guard let runtime = gone.runtime_id else { throw APIError.decoding("session_gone without runtime_id") }
            change = .sessionGone(FleetSessionKey(runtimeID: runtime, id: gone.id))
        case "runtime_gone": change = .runtimeGone(id: try decoder.decode(Gone.self, from: data).id)
        default: return nil
        }
        if case .fleet(let fleet) = change { return (change, fleet.rev) }
        guard let rev = event.id.flatMap(Int.init) else { throw APIError.decoding("fleet event without a rev") }
        return (change, rev)
    }
}
