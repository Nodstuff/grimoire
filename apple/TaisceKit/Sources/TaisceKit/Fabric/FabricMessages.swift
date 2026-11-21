import Foundation

// The signed Agent Fabric messages (`proto/taisce/runtime/v1/runtime.proto`):
// what the app builds and signs (Order, Settings, PairingClaim, PairingSeal)
// and what it verifies and reads (AttentionRequest). `encoded()` is the
// canonical encoding SIGNING asks for, checked byte for byte against
// `testdata/signing-vectors.json`.

/// What a `Command` does to its session (`Verb` in the .proto).
public enum FabricVerb: Int, Sendable, Hashable, CaseIterable, Codable {
    case send = 1
    case interrupt = 2
    case approve = 3
    case deny = 4
    case resume = 5
    // 6 is spawn, reserved for Slice 6
    case seen = 7
    case answer = 8
    case cancel = 9
    case reorder = 10
    case releaseQueue = 11
    case wake = 12
    case setFabric = 13
    case compact = 14
    case clear = 15

    /// The server's word for it (an order's `verb` on the viewer API).
    public var name: String {
        switch self {
        case .send: "send"
        case .interrupt: "interrupt"
        case .approve: "approve"
        case .deny: "deny"
        case .resume: "resume"
        case .seen: "seen"
        case .answer: "answer"
        case .cancel: "cancel"
        case .reorder: "reorder"
        case .releaseQueue: "release_queue"
        case .wake: "wake"
        case .setFabric: "set_fabric"
        case .compact: "compact"
        case .clear: "clear"
        }
    }

    public init?(name: String) {
        guard let v = Self.allCases.first(where: { $0.name == name }) else { return nil }
        self = v
    }
}

/// Act on one session.
public struct FabricCommand: Sendable, Hashable {
    public var sessionID: String
    public var verb: FabricVerb
    /// SEND, ANSWER: the words to type. DENY: what the session is told.
    /// SET_FABRIC: the fabric's id, or empty for the project's own.
    public var text: String
    /// SEND: the turn the person was reading
    public var afterTurn: String
    /// SEND: queue it behind whatever runs, rather than refuse it when the
    /// turn moved on
    public var queued: Bool
    /// APPROVE, DENY, ANSWER
    public var requestID: String
    /// APPROVE, DENY, ANSWER: SHA-256 of the request bytes shown
    public var requestHash: Data
    /// ANSWER: the choice tapped
    public var choiceID: String
    /// CANCEL, REORDER
    public var orderIDs: [String]

    public init(sessionID: String, verb: FabricVerb, text: String = "", afterTurn: String = "", queued: Bool = false,
                requestID: String = "", requestHash: Data = Data(), choiceID: String = "", orderIDs: [String] = []) {
        self.sessionID = sessionID
        self.verb = verb
        self.text = text
        self.afterTurn = afterTurn
        self.queued = queued
        self.requestID = requestID
        self.requestHash = requestHash
        self.choiceID = choiceID
        self.orderIDs = orderIDs
    }

    func encoded() -> Data {
        var w = ProtoWriter()
        w.string(1, sessionID)
        w.uint(2, UInt64(verb.rawValue))
        w.string(3, text)
        w.string(4, afterTurn)
        w.bool(5, queued)
        w.string(6, requestID)
        w.bytes(7, requestHash)
        w.string(8, choiceID)
        for id in orderIDs { w.string(9, id) }
        return w.data
    }

    static func decode(_ data: Data) throws -> FabricCommand {
        var r = ProtoReader(data)
        var c = FabricCommand(sessionID: "", verb: .send)
        var verb = 0
        while !r.isAtEnd {
            let (f, wire) = try r.next()
            switch f {
            case 1: c.sessionID = try r.string(field: f, wire: wire)
            case 2: verb = Int(try r.uint(field: f, wire: wire))
            case 3: c.text = try r.string(field: f, wire: wire)
            case 4: c.afterTurn = try r.string(field: f, wire: wire)
            case 5: c.queued = try r.bool(field: f, wire: wire)
            case 6: c.requestID = try r.string(field: f, wire: wire)
            case 7: c.requestHash = try r.lengthDelimited(field: f, wire: wire)
            case 8: c.choiceID = try r.string(field: f, wire: wire)
            case 9: c.orderIDs.append(try r.string(field: f, wire: wire))
            default: throw ProtoError.unknownField(f)
            }
        }
        guard let v = FabricVerb(rawValue: verb) else { throw ProtoError.unknownField(2) }
        c.verb = v
        return c
    }
}

/// An order to one runtime: a command to one of its sessions, or Hold all
/// (or its release).
public struct FabricOrder: Sendable, Hashable {
    public enum What: Sendable, Hashable {
        case command(FabricCommand)
        /// Hold all on (true) or off
        case hold(Bool)
    }

    public var runtimeID: String
    public var counter: UInt64
    public var expiresAt: Int64
    public var what: What
    public var orderID: String
    public var issuedAt: Int64

    public init(runtimeID: String, counter: UInt64, expiresAt: Int64, what: What, orderID: String, issuedAt: Int64) {
        self.runtimeID = runtimeID
        self.counter = counter
        self.expiresAt = expiresAt
        self.what = what
        self.orderID = orderID
        self.issuedAt = issuedAt
    }

    /// The canonical bytes that are signed and carried.
    public func encoded() -> Data {
        var w = ProtoWriter()
        w.string(1, runtimeID)
        w.uint(2, counter)
        w.int(3, expiresAt)
        if case .command(let c) = what { w.message(4, c.encoded()) }
        if case .hold(let on) = what {
            var h = ProtoWriter()
            h.bool(1, on)
            w.message(7, h.data)
        }
        w.string(9, orderID)
        w.int(10, issuedAt)
        return w.data
    }

    /// Strict: an unknown field (or verb) refuses the order. For checking
    /// what was signed (tests, and a stand-in server); the app never needs
    /// to read an order back.
    public static func decode(_ data: Data) throws -> FabricOrder {
        var r = ProtoReader(data)
        var o = FabricOrder(runtimeID: "", counter: 0, expiresAt: 0, what: .hold(false), orderID: "", issuedAt: 0)
        var what: What?
        while !r.isAtEnd {
            let (f, wire) = try r.next()
            switch f {
            case 1: o.runtimeID = try r.string(field: f, wire: wire)
            case 2: o.counter = try r.uint(field: f, wire: wire)
            case 3: o.expiresAt = try r.int(field: f, wire: wire)
            case 4: what = .command(try FabricCommand.decode(try r.lengthDelimited(field: f, wire: wire)))
            case 7:
                var h = ProtoReader(try r.lengthDelimited(field: f, wire: wire))
                var on = false
                while !h.isAtEnd {
                    let (hf, hw) = try h.next()
                    guard hf == 1 else { throw ProtoError.unknownField(hf) }
                    on = try h.bool(field: hf, wire: hw)
                }
                what = .hold(on)
            case 9: o.orderID = try r.string(field: f, wire: wire)
            case 10: o.issuedAt = try r.int(field: f, wire: wire)
            default: throw ProtoError.unknownField(f)
            }
        }
        guard let what else { throw ProtoError.truncated }
        o.what = what
        return o
    }

    /// The bytes decode, and re-encode to themselves (SIGNING's canonical
    /// form; the hub refuses anything else as `non_canonical`).
    public static func isCanonical(_ data: Data) -> Bool {
        (try? decode(data).encoded()) == data
    }
}

/// A kind of action that waits for the person's approval.
public struct FabricGuardrail: Sendable, Hashable, Codable, Identifiable {
    public var id: String
    public var label: String
    public var enabled: Bool
    /// empty means ["Bash"]; "*" every tool
    public var tools: [String]
    public var pattern: String
    /// a check the runtime implements itself (`delete_outside_worktree`)
    public var builtin: String

    public init(id: String, label: String, enabled: Bool = true, tools: [String] = ["Bash"], pattern: String = "", builtin: String = "") {
        self.id = id
        self.label = label
        self.enabled = enabled
        self.tools = tools
        self.pattern = pattern
        self.builtin = builtin
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decodeIfPresent(String.self, forKey: .id) ?? ""
        label = try c.decodeIfPresent(String.self, forKey: .label) ?? ""
        enabled = try c.decodeIfPresent(Bool.self, forKey: .enabled) ?? false
        tools = try c.decodeIfPresent([String].self, forKey: .tools) ?? []
        pattern = try c.decodeIfPresent(String.self, forKey: .pattern) ?? ""
        builtin = try c.decodeIfPresent(String.self, forKey: .builtin) ?? ""
    }

    func encoded() -> Data {
        var w = ProtoWriter()
        w.string(1, id)
        w.string(2, label)
        w.bool(3, enabled)
        for t in tools { w.string(4, t) }
        w.string(5, pattern)
        w.string(6, builtin)
        return w.data
    }
}

/// What agents may spend without the person. 0 means the default.
public struct FabricLimits: Sendable, Hashable, Codable {
    public var pairMessages: Int
    public var agentTurnsPerHour: Int
    public var wakesPerSessionPerHour: Int
    public var coldContextPct: Int
    public var coldIdleMinutes: Int

    public static let defaults = FabricLimits(pairMessages: 12, agentTurnsPerHour: 20, wakesPerSessionPerHour: 4, coldContextPct: 80, coldIdleMinutes: 30)

    public init(pairMessages: Int = 0, agentTurnsPerHour: Int = 0, wakesPerSessionPerHour: Int = 0, coldContextPct: Int = 0, coldIdleMinutes: Int = 0) {
        self.pairMessages = pairMessages
        self.agentTurnsPerHour = agentTurnsPerHour
        self.wakesPerSessionPerHour = wakesPerSessionPerHour
        self.coldContextPct = coldContextPct
        self.coldIdleMinutes = coldIdleMinutes
    }

    enum CodingKeys: String, CodingKey {
        case pairMessages = "pair_messages"
        case agentTurnsPerHour = "agent_turns_per_hour"
        case wakesPerSessionPerHour = "wakes_per_session_per_hour"
        case coldContextPct = "cold_context_pct"
        case coldIdleMinutes = "cold_idle_minutes"
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        pairMessages = try c.decodeIfPresent(Int.self, forKey: .pairMessages) ?? 0
        agentTurnsPerHour = try c.decodeIfPresent(Int.self, forKey: .agentTurnsPerHour) ?? 0
        wakesPerSessionPerHour = try c.decodeIfPresent(Int.self, forKey: .wakesPerSessionPerHour) ?? 0
        coldContextPct = try c.decodeIfPresent(Int.self, forKey: .coldContextPct) ?? 0
        coldIdleMinutes = try c.decodeIfPresent(Int.self, forKey: .coldIdleMinutes) ?? 0
    }

    /// Each limit at its effective value (0 is the default).
    public var effective: FabricLimits {
        let d = Self.defaults
        return FabricLimits(
            pairMessages: pairMessages == 0 ? d.pairMessages : pairMessages,
            agentTurnsPerHour: agentTurnsPerHour == 0 ? d.agentTurnsPerHour : agentTurnsPerHour,
            wakesPerSessionPerHour: wakesPerSessionPerHour == 0 ? d.wakesPerSessionPerHour : wakesPerSessionPerHour,
            coldContextPct: coldContextPct == 0 ? d.coldContextPct : coldContextPct,
            coldIdleMinutes: coldIdleMinutes == 0 ? d.coldIdleMinutes : coldIdleMinutes
        )
    }

    func encoded() -> Data {
        var w = ProtoWriter()
        w.uint(1, UInt64(max(0, pairMessages)))
        w.uint(2, UInt64(max(0, agentTurnsPerHour)))
        w.uint(3, UInt64(max(0, wakesPerSessionPerHour)))
        w.uint(4, UInt64(max(0, coldContextPct)))
        w.uint(5, UInt64(max(0, coldIdleMinutes)))
        return w.data
    }
}

/// A named group of sessions working on one thing.
public struct FabricDefinition: Sendable, Hashable, Codable, Identifiable {
    public var id: String
    public var name: String
    public var purpose: String
    public var docID: String
    public var docTitle: String

    public init(id: String, name: String, purpose: String = "", docID: String = "", docTitle: String = "") {
        self.id = id
        self.name = name
        self.purpose = purpose
        self.docID = docID
        self.docTitle = docTitle
    }

    enum CodingKeys: String, CodingKey {
        case id, name, purpose
        case docID = "doc_id"
        case docTitle = "doc_title"
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decodeIfPresent(String.self, forKey: .id) ?? ""
        name = try c.decodeIfPresent(String.self, forKey: .name) ?? ""
        purpose = try c.decodeIfPresent(String.self, forKey: .purpose) ?? ""
        docID = try c.decodeIfPresent(String.self, forKey: .docID) ?? ""
        docTitle = try c.decodeIfPresent(String.self, forKey: .docTitle) ?? ""
    }

    func encoded() -> Data {
        var w = ProtoWriter()
        w.string(1, id)
        w.string(2, name)
        w.string(3, purpose)
        w.string(4, docID)
        w.string(5, docTitle)
        return w.data
    }
}

/// The person's standing configuration, whole and versioned: guardrails,
/// limits and fabrics.
public struct FabricSettings: Sendable, Hashable, Codable {
    public var version: UInt64
    public var issuedAt: Int64
    public var guardrails: [FabricGuardrail]
    /// nil: not set (every limit at its default, not written)
    public var limits: FabricLimits?
    public var fabrics: [FabricDefinition]

    public init(version: UInt64, issuedAt: Int64 = 0, guardrails: [FabricGuardrail] = [], limits: FabricLimits? = nil, fabrics: [FabricDefinition] = []) {
        self.version = version
        self.issuedAt = issuedAt
        self.guardrails = guardrails
        self.limits = limits
        self.fabrics = fabrics
    }

    enum CodingKeys: String, CodingKey {
        case version, guardrails, limits, fabrics
        case issuedAt = "issued_at"
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        version = try c.decodeIfPresent(UInt64.self, forKey: .version) ?? 0
        issuedAt = try c.decodeIfPresent(Int64.self, forKey: .issuedAt) ?? 0
        guardrails = try c.decodeIfPresent([FabricGuardrail].self, forKey: .guardrails) ?? []
        limits = try c.decodeIfPresent(FabricLimits.self, forKey: .limits)
        fabrics = try c.decodeIfPresent([FabricDefinition].self, forKey: .fabrics) ?? []
    }

    /// The canonical bytes that are signed and carried.
    public func encoded() -> Data {
        var w = ProtoWriter()
        w.uint(1, version)
        w.int(2, issuedAt)
        for g in guardrails { w.message(3, g.encoded()) }
        if let limits { w.message(4, limits.encoded()) }
        for f in fabrics { w.message(5, f.encoded()) }
        return w.data
    }
}

/// What kind of thing waits for the person.
public enum AttentionKind: Int, Sendable, Hashable, Codable {
    case unspecified = 0
    /// a guardrail holds a tool call: approve or deny
    case approval = 1
    /// the agent asked: answer
    case question = 2
    /// Claude Code shows a dialog that needs someone at the Mac
    case dialog = 3
    /// the runtime saw a session switch its hooks off and holds it
    case tamper = 4
}

/// A button on a question.
public struct FabricChoice: Sendable, Hashable, Codable, Identifiable {
    public var id: String
    public var label: String

    public init(id: String, label: String) {
        self.id = id
        self.label = label
    }

    func encoded() -> Data {
        var w = ProtoWriter()
        w.string(1, id)
        w.string(2, label)
        return w.data
    }

    static func decode(_ data: Data) throws -> FabricChoice {
        var r = ProtoReader(data)
        var c = FabricChoice(id: "", label: "")
        while !r.isAtEnd {
            let (f, wire) = try r.next()
            switch f {
            case 1: c.id = try r.string(field: f, wire: wire)
            case 2: c.label = try r.string(field: f, wire: wire)
            default: throw ProtoError.unknownField(f)
            }
        }
        return c
    }
}

/// What a runtime signs when it raises an attention item. The app reads
/// only what it has verified: the text shown and approved is this, decoded
/// from the signed bytes, never the server's JSON beside it.
public struct AttentionRequest: Sendable, Hashable {
    public var runtimeID: String
    public var sessionID: String
    public var requestID: String
    public var kind: AttentionKind
    public var text: String
    public var choices: [FabricChoice]
    public var tool: String
    public var guardrailID: String
    public var guardrailLabel: String
    public var turnID: String
    public var raisedAt: Int64
    public var expiresAt: Int64

    public init(runtimeID: String, sessionID: String, requestID: String, kind: AttentionKind, text: String, choices: [FabricChoice] = [],
                tool: String = "", guardrailID: String = "", guardrailLabel: String = "", turnID: String = "", raisedAt: Int64 = 0, expiresAt: Int64 = 0) {
        self.runtimeID = runtimeID
        self.sessionID = sessionID
        self.requestID = requestID
        self.kind = kind
        self.text = text
        self.choices = choices
        self.tool = tool
        self.guardrailID = guardrailID
        self.guardrailLabel = guardrailLabel
        self.turnID = turnID
        self.raisedAt = raisedAt
        self.expiresAt = expiresAt
    }

    public func encoded() -> Data {
        var w = ProtoWriter()
        w.string(1, runtimeID)
        w.string(2, sessionID)
        w.string(3, requestID)
        w.uint(4, UInt64(kind.rawValue))
        w.string(5, text)
        for c in choices { w.message(6, c.encoded()) }
        w.string(7, tool)
        w.string(8, guardrailID)
        w.string(9, guardrailLabel)
        w.string(10, turnID)
        w.int(11, raisedAt)
        w.int(12, expiresAt)
        return w.data
    }

    /// Strict: a field this build does not know refuses the whole request.
    public static func decode(_ data: Data) throws -> AttentionRequest {
        var r = ProtoReader(data)
        var a = AttentionRequest(runtimeID: "", sessionID: "", requestID: "", kind: .unspecified, text: "")
        while !r.isAtEnd {
            let (f, wire) = try r.next()
            switch f {
            case 1: a.runtimeID = try r.string(field: f, wire: wire)
            case 2: a.sessionID = try r.string(field: f, wire: wire)
            case 3: a.requestID = try r.string(field: f, wire: wire)
            case 4: a.kind = AttentionKind(rawValue: Int(try r.uint(field: f, wire: wire))) ?? .unspecified
            case 5: a.text = try r.string(field: f, wire: wire)
            case 6: a.choices.append(try FabricChoice.decode(try r.lengthDelimited(field: f, wire: wire)))
            case 7: a.tool = try r.string(field: f, wire: wire)
            case 8: a.guardrailID = try r.string(field: f, wire: wire)
            case 9: a.guardrailLabel = try r.string(field: f, wire: wire)
            case 10: a.turnID = try r.string(field: f, wire: wire)
            case 11: a.raisedAt = try r.int(field: f, wire: wire)
            case 12: a.expiresAt = try r.int(field: f, wire: wire)
            default: throw ProtoError.unknownField(f)
            }
        }
        return a
    }
}

/// What a device says about itself when it pairs.
public struct PairingClaim: Sendable, Hashable {
    public var runtimeID: String
    public var pairingID: String
    public var publicKey: Data
    public var deviceName: String
    public var createdAt: Int64

    public init(runtimeID: String, pairingID: String, publicKey: Data, deviceName: String, createdAt: Int64) {
        self.runtimeID = runtimeID
        self.pairingID = pairingID
        self.publicKey = publicKey
        self.deviceName = deviceName
        self.createdAt = createdAt
    }

    public func encoded() -> Data {
        var w = ProtoWriter()
        w.string(1, runtimeID)
        w.string(2, pairingID)
        w.bytes(3, publicKey)
        w.string(4, deviceName)
        w.int(5, createdAt)
        return w.data
    }
}

/// The claim and the device's signature over it, sealed for the runtime.
public struct PairingSeal: Sendable, Hashable {
    public var claim: Data
    public var signature: Data

    public func encoded() -> Data {
        var w = ProtoWriter()
        w.bytes(1, claim)
        w.bytes(2, signature)
        return w.data
    }
}
