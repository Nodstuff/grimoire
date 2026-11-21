import Foundation

// Agent Fabric Slices 2–5 (read, command, ask, collaborate): the viewer
// API's shapes (ADR 0008 §11). Every field is always present on the wire
// (`null` where none); decoding is tolerant all the same, so a server one
// step behind (Slice 1) or ahead (a new field, a new word) still reads.

extension KeyedDecodingContainer {
    /// The value, or `fallback` when the key is absent or null.
    func value<T: Decodable>(_ key: Key, _ fallback: T) throws -> T {
        try decodeIfPresent(T.self, forKey: key) ?? fallback
    }

    /// Standard base64 bytes; nil when absent, null or unreadable.
    func base64(_ key: Key) throws -> Data? {
        try decodeIfPresent(String.self, forKey: key).flatMap { Data(base64Encoded: $0) }
    }
}

/// A session named on the wire: `{runtime_id, session_id, address}`.
public struct FabricSessionRef: Sendable, Hashable, Codable {
    public var runtimeID: String
    public var sessionID: String
    public var address: String

    public init(runtimeID: String, sessionID: String, address: String) {
        self.runtimeID = runtimeID
        self.sessionID = sessionID
        self.address = address
    }

    public var key: FleetSessionKey { FleetSessionKey(runtimeID: runtimeID, id: sessionID) }

    enum CodingKeys: String, CodingKey {
        case runtimeID = "runtime_id"
        case sessionID = "session_id"
        case address
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        runtimeID = try c.value(.runtimeID, "")
        sessionID = try c.value(.sessionID, "")
        address = try c.value(.address, "")
    }
}

/// A device paired with a runtime at the Mac.
public struct FabricPairedDevice: Sendable, Hashable, Codable, Identifiable {
    public var deviceID: String
    public var fingerprint: String
    public var name: String
    public var publicKey: Data?
    public var pairedAt: Int
    /// the highest order counter the runtime has taken from it
    public var counter: UInt64

    public var id: String { deviceID }

    public init(deviceID: String, fingerprint: String = "", name: String = "", publicKey: Data? = nil, pairedAt: Int = 0, counter: UInt64 = 0) {
        self.deviceID = deviceID
        self.fingerprint = fingerprint
        self.name = name
        self.publicKey = publicKey
        self.pairedAt = pairedAt
        self.counter = counter
    }

    enum CodingKeys: String, CodingKey {
        case deviceID = "device_id"
        case fingerprint, name, counter
        case publicKey = "public_key"
        case pairedAt = "paired_at"
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        deviceID = try c.value(.deviceID, "")
        fingerprint = try c.value(.fingerprint, "")
        name = try c.value(.name, "")
        publicKey = try c.base64(.publicKey)
        pairedAt = try c.value(.pairedAt, 0)
        counter = try c.value(.counter, 0)
    }

    public func encode(to encoder: any Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(deviceID, forKey: .deviceID)
        try c.encode(fingerprint, forKey: .fingerprint)
        try c.encode(name, forKey: .name)
        try c.encode(publicKey?.base64EncodedString(), forKey: .publicKey)
        try c.encode(pairedAt, forKey: .pairedAt)
        try c.encode(counter, forKey: .counter)
    }
}

// MARK: turns

/// How a turn ended.
public enum TurnEnd: String, Sendable, Hashable, Codable {
    case clean, question, error, interrupted, denied, unknown

    public init(from decoder: any Decoder) throws {
        self = TurnEnd(rawValue: try decoder.singleValueContainer().decode(String.self)) ?? .unknown
    }
}

/// Where a turn's prompt came from.
public enum PromptSource: String, Sendable, Hashable, Codable {
    case mac, order, answer, wake, unknown

    public init(from decoder: any Decoder) throws {
        self = PromptSource(rawValue: try decoder.singleValueContainer().decode(String.self)) ?? .unknown
    }
}

/// What a turn changed, from git. Zero and empty when unknown.
public struct GitOutcome: Sendable, Hashable, Codable {
    public var filesChanged: Int
    public var linesAdded: Int
    public var linesRemoved: Int
    public var commits: Int
    public var pullRequestURL: String?
    public var pullRequestNumber: Int?

    public init(filesChanged: Int = 0, linesAdded: Int = 0, linesRemoved: Int = 0, commits: Int = 0, pullRequestURL: String? = nil, pullRequestNumber: Int? = nil) {
        self.filesChanged = filesChanged
        self.linesAdded = linesAdded
        self.linesRemoved = linesRemoved
        self.commits = commits
        self.pullRequestURL = pullRequestURL
        self.pullRequestNumber = pullRequestNumber
    }

    public var isEmpty: Bool { filesChanged == 0 && linesAdded == 0 && linesRemoved == 0 && commits == 0 && (pullRequestURL ?? "").isEmpty }

    enum CodingKeys: String, CodingKey {
        case filesChanged = "files_changed"
        case linesAdded = "lines_added"
        case linesRemoved = "lines_removed"
        case commits
        case pullRequestURL = "pull_request_url"
        case pullRequestNumber = "pull_request_number"
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        filesChanged = try c.value(.filesChanged, 0)
        linesAdded = try c.value(.linesAdded, 0)
        linesRemoved = try c.value(.linesRemoved, 0)
        commits = try c.value(.commits, 0)
        pullRequestURL = try c.decodeIfPresent(String.self, forKey: .pullRequestURL).flatMap { $0.isEmpty ? nil : $0 }
        pullRequestNumber = try c.decodeIfPresent(Int.self, forKey: .pullRequestNumber).flatMap { $0 == 0 ? nil : $0 }
    }
}

/// One finished turn of one session: the prompt, the reply, what changed.
public struct FabricTurn: Sendable, Hashable, Codable, Identifiable {
    public var runtimeID: String
    public var sessionID: String
    public var address: String
    public var turnID: String
    /// empty under Text off (on the routes; the live event still carries it)
    public var prompt: String
    public var reply: String
    public var toolCount: Int
    public var outcome: GitOutcome
    public var started: Int
    public var ended: Int
    public var truncated: Bool
    public var contextUsedPct: Int?
    public var end: TurnEnd
    public var source: PromptSource
    public var orderID: String?
    public var textOff: Bool

    public var id: String { turnID }
    public var session: FleetSessionKey { FleetSessionKey(runtimeID: runtimeID, id: sessionID) }

    public init(runtimeID: String, sessionID: String, address: String, turnID: String, prompt: String, reply: String, toolCount: Int = 0,
                outcome: GitOutcome = GitOutcome(), started: Int, ended: Int, truncated: Bool = false, contextUsedPct: Int? = nil,
                end: TurnEnd = .clean, source: PromptSource = .mac, orderID: String? = nil, textOff: Bool = false) {
        self.runtimeID = runtimeID
        self.sessionID = sessionID
        self.address = address
        self.turnID = turnID
        self.prompt = prompt
        self.reply = reply
        self.toolCount = toolCount
        self.outcome = outcome
        self.started = started
        self.ended = ended
        self.truncated = truncated
        self.contextUsedPct = contextUsedPct
        self.end = end
        self.source = source
        self.orderID = orderID
        self.textOff = textOff
    }

    enum CodingKeys: String, CodingKey {
        case runtimeID = "runtime_id"
        case sessionID = "session_id"
        case address
        case turnID = "turn_id"
        case prompt, reply
        case toolCount = "tool_count"
        case outcome, started, ended, truncated
        case contextUsedPct = "context_used_pct"
        case end, source
        case orderID = "order_id"
        case textOff = "text_off"
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        runtimeID = try c.value(.runtimeID, "")
        sessionID = try c.value(.sessionID, "")
        address = try c.value(.address, "")
        turnID = try c.value(.turnID, "")
        prompt = try c.value(.prompt, "")
        reply = try c.value(.reply, "")
        toolCount = try c.value(.toolCount, 0)
        outcome = try c.value(.outcome, GitOutcome())
        started = try c.value(.started, 0)
        ended = try c.value(.ended, 0)
        truncated = try c.value(.truncated, false)
        contextUsedPct = try c.decodeIfPresent(Int.self, forKey: .contextUsedPct)
        end = try c.value(.end, .unknown)
        source = try c.value(.source, .unknown)
        orderID = try c.decodeIfPresent(String.self, forKey: .orderID).flatMap { $0.isEmpty ? nil : $0 }
        textOff = try c.value(.textOff, false)
    }
}

/// `GET …/turns`: newest first.
public struct FabricTurnPage: Sendable, Hashable, Codable {
    public var turns: [FabricTurn]
    public var more: Bool

    public init(turns: [FabricTurn], more: Bool) {
        self.turns = turns
        self.more = more
    }
}

// MARK: attention

/// What an attention item asks of the person.
public enum FabricAttentionKind: String, Sendable, Hashable, Codable {
    /// a guardrail holds a tool call (runtime)
    case approval
    /// an agent asked (runtime)
    case question
    /// Claude Code shows a dialog at the Mac (runtime; informs only)
    case dialog
    /// a session switched its hooks off (runtime; informs only)
    case tamper
    /// mail for a session the cold rule kept asleep: wake it? (hub)
    case wakeOffer = "wake_offer"
    /// two sessions reached the pair limit: continue or stop? (hub)
    case pairPaused = "pair_paused"
    /// an ask to a session that is asleep or exited (hub)
    case askWaiting = "ask_waiting"
    case unknown

    public init(from decoder: any Decoder) throws {
        self = FabricAttentionKind(rawValue: try decoder.singleValueContainer().decode(String.self)) ?? .unknown
    }
}

/// The runtime's signed request, byte for byte.
public struct SignedAttention: Sendable, Hashable, Codable {
    public var request: Data
    public var signature: Data

    public init(request: Data, signature: Data) {
        self.request = request
        self.signature = signature
    }

    enum CodingKeys: String, CodingKey { case request, signature }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        request = try c.base64(.request) ?? Data()
        signature = try c.base64(.signature) ?? Data()
    }

    public func encode(to encoder: any Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(request.base64EncodedString(), forKey: .request)
        try c.encode(signature.base64EncodedString(), forKey: .signature)
    }
}

/// Something that waits for the person. A runtime's item (approval,
/// question, dialog, tamper) carries the runtime's signed request, which
/// the app verifies before showing or acting on it; the hub's (a wake
/// offer, a paused pair, an ask waiting) carry the conversation instead.
public struct FabricAttentionItem: Sendable, Hashable, Codable, Identifiable {
    public enum Source: String, Sendable, Hashable, Codable {
        case runtime, hub, unknown

        public init(from decoder: any Decoder) throws {
            self = Source(rawValue: try decoder.singleValueContainer().decode(String.self)) ?? .unknown
        }
    }

    public var id: String
    public var source: Source
    public var runtimeID: String
    public var sessionID: String
    public var address: String
    public var kind: FabricAttentionKind
    /// the hub's copy, for lists; what is approved is read from `signed`
    public var text: String
    public var choices: [FabricChoice]
    public var tool: String
    public var guardrailLabel: String
    public var turnID: String
    public var raisedAt: Int
    public var expiresAt: Int?
    public var signed: SignedAttention?
    public var conversationID: String?
    public var messageID: String?
    public var fromAddress: String?

    public var session: FleetSessionKey { FleetSessionKey(runtimeID: runtimeID, id: sessionID) }

    public init(id: String, source: Source, runtimeID: String, sessionID: String, address: String, kind: FabricAttentionKind, text: String,
                choices: [FabricChoice] = [], tool: String = "", guardrailLabel: String = "", turnID: String = "", raisedAt: Int,
                expiresAt: Int? = nil, signed: SignedAttention? = nil, conversationID: String? = nil, messageID: String? = nil, fromAddress: String? = nil) {
        self.id = id
        self.source = source
        self.runtimeID = runtimeID
        self.sessionID = sessionID
        self.address = address
        self.kind = kind
        self.text = text
        self.choices = choices
        self.tool = tool
        self.guardrailLabel = guardrailLabel
        self.turnID = turnID
        self.raisedAt = raisedAt
        self.expiresAt = expiresAt
        self.signed = signed
        self.conversationID = conversationID
        self.messageID = messageID
        self.fromAddress = fromAddress
    }

    enum CodingKeys: String, CodingKey {
        case id, source
        case runtimeID = "runtime_id"
        case sessionID = "session_id"
        case address, kind, text, choices, tool
        case guardrailLabel = "guardrail_label"
        case turnID = "turn_id"
        case raisedAt = "raised_at"
        case expiresAt = "expires_at"
        case signed
        case conversationID = "conversation_id"
        case messageID = "message_id"
        case fromAddress = "from_address"
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.value(.id, "")
        source = try c.value(.source, .unknown)
        runtimeID = try c.value(.runtimeID, "")
        sessionID = try c.value(.sessionID, "")
        address = try c.value(.address, "")
        kind = try c.value(.kind, .unknown)
        text = try c.value(.text, "")
        choices = try c.value(.choices, [])
        tool = try c.value(.tool, "")
        guardrailLabel = try c.value(.guardrailLabel, "")
        turnID = try c.value(.turnID, "")
        raisedAt = try c.value(.raisedAt, 0)
        expiresAt = try c.decodeIfPresent(Int.self, forKey: .expiresAt).flatMap { $0 == 0 ? nil : $0 }
        signed = try c.decodeIfPresent(SignedAttention.self, forKey: .signed)
        conversationID = try c.decodeIfPresent(String.self, forKey: .conversationID)
        messageID = try c.decodeIfPresent(String.self, forKey: .messageID)
        fromAddress = try c.decodeIfPresent(String.self, forKey: .fromAddress)
    }

    /// Past its deadline at `now`.
    public func isExpired(at now: Date = .now) -> Bool {
        expiresAt.map { Double($0) <= now.timeIntervalSince1970 } ?? false
    }

    /// Something the person can act on from the phone (dialog and tamper inform only).
    public var isActionable: Bool {
        switch kind {
        case .approval, .question, .wakeOffer, .pairPaused, .askWaiting: true
        default: false
        }
    }
}

/// `attention_cleared`: an item no longer waits.
public struct FabricAttentionCleared: Sendable, Hashable, Codable {
    public var id: String
    public var runtimeID: String
    public var sessionID: String
    public var resolution: String

    public init(id: String, runtimeID: String = "", sessionID: String = "", resolution: String = "") {
        self.id = id
        self.runtimeID = runtimeID
        self.sessionID = sessionID
        self.resolution = resolution
    }

    enum CodingKeys: String, CodingKey {
        case id
        case runtimeID = "runtime_id"
        case sessionID = "session_id"
        case resolution
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.value(.id, "")
        runtimeID = try c.value(.runtimeID, "")
        sessionID = try c.value(.sessionID, "")
        resolution = try c.value(.resolution, "")
    }
}

// MARK: orders

/// Where an order is.
public enum FabricOrderState: String, Sendable, Hashable, Codable {
    /// held at the hub, not yet on the stream (can be withdrawn)
    case pending
    /// on the stream
    case sent
    /// the runtime has it, no result yet
    case received
    case queued, done, refused, failed
    /// its time passed before the runtime received it
    case expired
    case withdrawn
    case unknown

    public init(from decoder: any Decoder) throws {
        self = FabricOrderState(rawValue: try decoder.singleValueContainer().decode(String.self)) ?? .unknown
    }

    /// Nothing more will happen to it.
    public var isFinal: Bool {
        switch self {
        case .done, .refused, .failed, .expired, .withdrawn: true
        default: false
        }
    }
}

/// One order, as the hub holds it: the audit trail's row.
public struct FabricOrderView: Sendable, Hashable, Codable, Identifiable {
    public var id: String
    public var runtimeID: String
    public var sessionID: String?
    public var deviceID: String
    public var counter: UInt64
    /// `command` or `hold`
    public var kind: String
    public var verb: String?
    public var hold: Bool?
    public var text: String?
    public var afterTurn: String?
    public var queued: Bool
    public var requestID: String?
    public var choiceID: String?
    public var orderIDs: [String]
    public var issuedAt: Int
    public var expiresAt: Int
    public var createdAt: Int
    public var updatedAt: Int
    public var state: FabricOrderState
    public var reason: String?
    public var detail: String?

    public var fabricVerb: FabricVerb? { verb.flatMap(FabricVerb.init(name:)) }

    public init(id: String, runtimeID: String, sessionID: String?, deviceID: String = "", counter: UInt64 = 0, kind: String = "command", verb: String?,
                hold: Bool? = nil, text: String? = nil, afterTurn: String? = nil, queued: Bool = false, requestID: String? = nil, choiceID: String? = nil,
                orderIDs: [String] = [], issuedAt: Int = 0, expiresAt: Int = 0, createdAt: Int = 0, updatedAt: Int = 0, state: FabricOrderState,
                reason: String? = nil, detail: String? = nil) {
        self.id = id
        self.runtimeID = runtimeID
        self.sessionID = sessionID
        self.deviceID = deviceID
        self.counter = counter
        self.kind = kind
        self.verb = verb
        self.hold = hold
        self.text = text
        self.afterTurn = afterTurn
        self.queued = queued
        self.requestID = requestID
        self.choiceID = choiceID
        self.orderIDs = orderIDs
        self.issuedAt = issuedAt
        self.expiresAt = expiresAt
        self.createdAt = createdAt
        self.updatedAt = updatedAt
        self.state = state
        self.reason = reason
        self.detail = detail
    }

    enum CodingKeys: String, CodingKey {
        case id
        case runtimeID = "runtime_id"
        case sessionID = "session_id"
        case deviceID = "device_id"
        case counter, kind, verb, hold, text
        case afterTurn = "after_turn"
        case queued
        case requestID = "request_id"
        case choiceID = "choice_id"
        case orderIDs = "order_ids"
        case issuedAt = "issued_at"
        case expiresAt = "expires_at"
        case createdAt = "created_at"
        case updatedAt = "updated_at"
        case state, reason, detail
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.value(.id, "")
        runtimeID = try c.value(.runtimeID, "")
        sessionID = try c.decodeIfPresent(String.self, forKey: .sessionID)
        deviceID = try c.value(.deviceID, "")
        counter = try c.value(.counter, 0)
        kind = try c.value(.kind, "command")
        verb = try c.decodeIfPresent(String.self, forKey: .verb)
        hold = try c.decodeIfPresent(Bool.self, forKey: .hold)
        text = try c.decodeIfPresent(String.self, forKey: .text)
        afterTurn = try c.decodeIfPresent(String.self, forKey: .afterTurn)
        queued = try c.value(.queued, false)
        requestID = try c.decodeIfPresent(String.self, forKey: .requestID)
        choiceID = try c.decodeIfPresent(String.self, forKey: .choiceID)
        orderIDs = try c.value(.orderIDs, [])
        issuedAt = try c.value(.issuedAt, 0)
        expiresAt = try c.value(.expiresAt, 0)
        createdAt = try c.value(.createdAt, 0)
        updatedAt = try c.value(.updatedAt, 0)
        state = try c.value(.state, .unknown)
        reason = try c.decodeIfPresent(String.self, forKey: .reason)
        detail = try c.decodeIfPresent(String.self, forKey: .detail)
    }
}

/// `POST /api/fabric/orders` and `PUT /api/fabric/settings`: a signed payload.
public struct FabricSignedBody: Sendable, Hashable, Encodable {
    public var payload: Data
    public var deviceID: String
    public var signature: Data

    public init(payload: Data, deviceID: String, signature: Data) {
        self.payload = payload
        self.deviceID = deviceID
        self.signature = signature
    }

    enum CodingKeys: String, CodingKey {
        case payload
        case deviceID = "device_id"
        case signature
    }

    public func encode(to encoder: any Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(payload.base64EncodedString(), forKey: .payload)
        try c.encode(deviceID, forKey: .deviceID)
        try c.encode(signature.base64EncodedString(), forKey: .signature)
    }
}

// MARK: settings, fabrics, projects

/// `GET /api/fabric/settings`.
public struct FabricSettingsView: Sendable, Hashable, Codable {
    public struct RuntimeVersion: Sendable, Hashable, Codable {
        public var runtimeID: String
        public var settingsVersion: UInt64

        enum CodingKeys: String, CodingKey {
            case runtimeID = "runtime_id"
            case settingsVersion = "settings_version"
        }

        public init(runtimeID: String, settingsVersion: UInt64) {
            self.runtimeID = runtimeID
            self.settingsVersion = settingsVersion
        }
    }

    public var version: UInt64
    public var issuedAt: Int
    /// nothing has been signed yet: `settings` are the built-in defaults
    public var defaults: Bool
    public var settings: FabricSettings
    /// the version each runtime has in force
    public var runtimes: [RuntimeVersion]

    public init(version: UInt64, issuedAt: Int = 0, defaults: Bool, settings: FabricSettings, runtimes: [RuntimeVersion] = []) {
        self.version = version
        self.issuedAt = issuedAt
        self.defaults = defaults
        self.settings = settings
        self.runtimes = runtimes
    }

    enum CodingKeys: String, CodingKey {
        case version
        case issuedAt = "issued_at"
        case defaults, settings, runtimes
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        version = try c.value(.version, 0)
        issuedAt = try c.value(.issuedAt, 0)
        defaults = try c.value(.defaults, version == 0)
        settings = try c.value(.settings, FabricDefaults.settings)
        runtimes = try c.value(.runtimes, [])
    }
}

/// A project's two switches.
public struct FabricProject: Sendable, Hashable, Codable, Identifiable {
    public var name: String
    /// turns, attention text and last lines are not stored
    public var textOff: Bool
    /// notifications say the state, never the text
    public var privateNotifications: Bool
    /// how many of its sessions the fleet has
    public var sessions: Int

    public var id: String { name }

    public init(name: String, textOff: Bool = false, privateNotifications: Bool = false, sessions: Int = 0) {
        self.name = name
        self.textOff = textOff
        self.privateNotifications = privateNotifications
        self.sessions = sessions
    }

    enum CodingKeys: String, CodingKey {
        case name
        case textOff = "text_off"
        case privateNotifications = "private_notifications"
        case sessions
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        name = try c.value(.name, "")
        textOff = try c.value(.textOff, false)
        privateNotifications = try c.value(.privateNotifications, false)
        // a count, or the sessions themselves
        if let n = try? c.decodeIfPresent(Int.self, forKey: .sessions) {
            sessions = n
        } else if let list = try? c.decodeIfPresent([FabricSessionRef].self, forKey: .sessions) {
            sessions = list.count
        } else {
            sessions = 0
        }
    }
}

/// A pairing offer's progress.
public struct FabricPairingView: Sendable, Hashable, Codable {
    public enum State: String, Sendable, Hashable, Codable {
        /// at the hub
        case pending
        /// on the stream to the Mac
        case sent
        /// confirmed at the Mac
        case paired
        /// the person at the Mac said no, or cancelled
        case rejected
        /// no pairing with that id is waiting (the code lived 10 minutes)
        case expired
        /// the offer did not open or verify
        case invalid
        case unknown

        public init(from decoder: any Decoder) throws {
            self = State(rawValue: try decoder.singleValueContainer().decode(String.self)) ?? .unknown
        }

        public var isFinal: Bool { self == .paired || self == .rejected || self == .expired || self == .invalid }
    }

    public var pairingID: String
    public var runtimeID: String
    public var state: State
    public var deviceID: String?
    public var detail: String?

    public init(pairingID: String, runtimeID: String, state: State, deviceID: String? = nil, detail: String? = nil) {
        self.pairingID = pairingID
        self.runtimeID = runtimeID
        self.state = state
        self.deviceID = deviceID
        self.detail = detail
    }

    enum CodingKeys: String, CodingKey {
        case pairingID = "pairing_id"
        case runtimeID = "runtime_id"
        case state
        case deviceID = "device_id"
        case detail
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        pairingID = try c.value(.pairingID, "")
        runtimeID = try c.value(.runtimeID, "")
        state = try c.value(.state, .unknown)
        deviceID = try c.decodeIfPresent(String.self, forKey: .deviceID)
        detail = try c.decodeIfPresent(String.self, forKey: .detail)
    }
}

// MARK: conversations

/// Two sessions talking.
public struct FabricConversation: Sendable, Hashable, Codable, Identifiable {
    public var id: String
    public var a: FabricSessionRef
    public var b: FabricSessionRef
    /// messages since the person last ordered either (the pair limit counts these)
    public var count: Int
    public var total: Int
    public var lastAt: Int
    /// the pair limit was reached: held until the person says continue or stop
    public var paused: Bool
    public var unansweredAsks: Int

    public init(id: String, a: FabricSessionRef, b: FabricSessionRef, count: Int = 0, total: Int = 0, lastAt: Int = 0, paused: Bool = false, unansweredAsks: Int = 0) {
        self.id = id
        self.a = a
        self.b = b
        self.count = count
        self.total = total
        self.lastAt = lastAt
        self.paused = paused
        self.unansweredAsks = unansweredAsks
    }

    enum CodingKeys: String, CodingKey {
        case id, a, b, count, total
        case lastAt = "last_at"
        case paused
        case unansweredAsks = "unanswered_asks"
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.value(.id, "")
        a = try c.value(.a, FabricSessionRef(runtimeID: "", sessionID: "", address: ""))
        b = try c.value(.b, FabricSessionRef(runtimeID: "", sessionID: "", address: ""))
        count = try c.value(.count, 0)
        total = try c.value(.total, 0)
        lastAt = try c.value(.lastAt, 0)
        paused = try c.value(.paused, false)
        unansweredAsks = try c.value(.unansweredAsks, 0)
    }
}

public enum FabricMessageKind: String, Sendable, Hashable, Codable {
    case note, ask, reply, bounce, unknown

    public init(from decoder: any Decoder) throws {
        self = FabricMessageKind(rawValue: try decoder.singleValueContainer().decode(String.self)) ?? .unknown
    }
}

/// One message between two sessions. Its body is gone once read.
public struct FabricMessage: Sendable, Hashable, Codable, Identifiable {
    public enum State: String, Sendable, Hashable, Codable {
        case unread, read, held, bounced, unknown

        public init(from decoder: any Decoder) throws {
            self = State(rawValue: try decoder.singleValueContainer().decode(String.self)) ?? .unknown
        }
    }

    public var id: String
    public var conversationID: String
    public var from: FabricSessionRef
    public var to: FabricSessionRef
    public var kind: FabricMessageKind
    /// nil once read, expired, or under Text off
    public var body: String?
    public var doc: String?
    public var replyTo: String?
    public var sentAt: Int
    public var readAt: Int?
    public var state: State
    public var bounceReason: String?
    /// an ask that has had its reply
    public var answered: Bool

    public init(id: String, conversationID: String, from: FabricSessionRef, to: FabricSessionRef, kind: FabricMessageKind, body: String?, doc: String? = nil,
                replyTo: String? = nil, sentAt: Int, readAt: Int? = nil, state: State, bounceReason: String? = nil, answered: Bool = false) {
        self.id = id
        self.conversationID = conversationID
        self.from = from
        self.to = to
        self.kind = kind
        self.body = body
        self.doc = doc
        self.replyTo = replyTo
        self.sentAt = sentAt
        self.readAt = readAt
        self.state = state
        self.bounceReason = bounceReason
        self.answered = answered
    }

    enum CodingKeys: String, CodingKey {
        case id
        case conversationID = "conversation_id"
        case from, to, kind, body, doc
        case replyTo = "reply_to"
        case inReplyTo = "in_reply_to"
        case sentAt = "sent_at"
        case readAt = "read_at"
        case state
        case bounceReason = "bounce_reason"
        case answered
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.value(.id, "")
        conversationID = try c.value(.conversationID, "")
        from = try c.value(.from, FabricSessionRef(runtimeID: "", sessionID: "", address: ""))
        to = try c.value(.to, FabricSessionRef(runtimeID: "", sessionID: "", address: ""))
        kind = try c.value(.kind, .unknown)
        body = try c.decodeIfPresent(String.self, forKey: .body)
        doc = try c.decodeIfPresent(String.self, forKey: .doc)
        // the contract renamed in_reply_to to reply_to (2026-11-21); either reads
        replyTo = try c.decodeIfPresent(String.self, forKey: .replyTo) ?? c.decodeIfPresent(String.self, forKey: .inReplyTo)
        sentAt = try c.value(.sentAt, 0)
        readAt = try c.decodeIfPresent(Int.self, forKey: .readAt)
        state = try c.value(.state, .unknown)
        bounceReason = try c.decodeIfPresent(String.self, forKey: .bounceReason)
        answered = try c.value(.answered, false)
    }

    public func encode(to encoder: any Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(id, forKey: .id)
        try c.encode(conversationID, forKey: .conversationID)
        try c.encode(from, forKey: .from)
        try c.encode(to, forKey: .to)
        try c.encode(kind, forKey: .kind)
        try c.encode(body, forKey: .body)
        try c.encode(doc, forKey: .doc)
        try c.encode(replyTo, forKey: .replyTo)
        try c.encode(sentAt, forKey: .sentAt)
        try c.encode(readAt, forKey: .readAt)
        try c.encode(state, forKey: .state)
        try c.encode(bounceReason, forKey: .bounceReason)
        try c.encode(answered, forKey: .answered)
    }
}

/// `GET /api/fabric/conversations/{id}`: newest first.
public struct FabricConversationPage: Sendable, Hashable, Codable {
    public var conversation: FabricConversation
    public var messages: [FabricMessage]

    public init(conversation: FabricConversation, messages: [FabricMessage]) {
        self.conversation = conversation
        self.messages = messages
    }
}

// MARK: errors

/// A refusal from a fabric route: `{"error": "<words>", "code": "<code>"}`
/// with its status (ADR 0008 §11: `replayed`, `bad_signature`,
/// `stale_settings`, `already_sent`, …).
public struct FabricError: Error, Sendable, Hashable {
    public var status: Int
    public var code: String
    public var message: String
    /// `stale_settings`: the version the server holds
    public var version: UInt64?

    public init(status: Int, code: String, message: String, version: UInt64? = nil) {
        self.status = status
        self.code = code
        self.message = message
        self.version = version
    }
}

// MARK: the stream's new events

/// One of the stream's Slice 2–5 events (ADR 0008 §11). They change no
/// fleet `rev`; each says the whole of one thing.
public enum FabricStreamEvent: Sendable, Hashable {
    /// a finished turn, text included even under Text off (the live view)
    case turn(FabricTurn)
    case attention(FabricAttentionItem)
    case attentionCleared(FabricAttentionCleared)
    case order(FabricOrderView)
    case settings(FabricSettingsView)
    case message(FabricMessage)
    case conversation(FabricConversation)
    case pairing(FabricPairingView)
    case project(FabricProject)

    /// Nil for an event that is not one of these (a fleet event, or one
    /// this build has no name for); throws when a known one cannot be read.
    public static func decode(_ event: SSEEvent) throws -> FabricStreamEvent? {
        let data = Data(event.data.utf8)
        let d = JSONDecoder()
        switch event.event {
        case "turn": return .turn(try d.decode(FabricTurn.self, from: data))
        case "attention": return .attention(try d.decode(FabricAttentionItem.self, from: data))
        case "attention_cleared": return .attentionCleared(try d.decode(FabricAttentionCleared.self, from: data))
        case "order": return .order(try d.decode(FabricOrderView.self, from: data))
        case "settings": return .settings(try d.decode(FabricSettingsView.self, from: data))
        case "message": return .message(try d.decode(FabricMessage.self, from: data))
        case "conversation": return .conversation(try d.decode(FabricConversation.self, from: data))
        case "pairing": return .pairing(try d.decode(FabricPairingView.self, from: data))
        case "project": return .project(try d.decode(FabricProject.self, from: data))
        default: return nil
        }
    }
}
