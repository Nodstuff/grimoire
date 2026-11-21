import CryptoKit
import Foundation
import Synchronization
import Valet

/// Why the app will not sign an order (before anything is sent).
public enum FabricOrderProblem: Error, Equatable, Sendable {
    /// SEND and ANSWER text never starts with `!` (a shell command no hook
    /// sees) or `/` (a slash command)
    case unsafeText
    case emptyText
    /// over 16 KiB
    case tooLong
    /// the request did not verify, or says something other than the item
    case unverified(String)
    /// this phone is not paired with the session's Mac
    case notPaired
    /// the verified request is not something this verb answers (an approval
    /// is approved or denied, a question answered)
    case wrongKind
}

/// Where this device keeps the highest order counter it has used.
public protocol FabricCounterStore: Sendable {
    func lastCounter() -> UInt64
    func setLastCounter(_ value: UInt64)
}

public final class MemoryCounterStore: FabricCounterStore {
    private let value = Mutex<UInt64>(0)
    public init(_ start: UInt64 = 0) { value.withLock { $0 = start } }
    public func lastCounter() -> UInt64 { value.withLock { $0 } }
    public func setLastCounter(_ v: UInt64) { value.withLock { $0 = v } }
}

public struct DefaultsCounterStore: FabricCounterStore {
    let key: String
    public init(key: String = "fabric.lastOrderCounter") { self.key = key }
    public func lastCounter() -> UInt64 { UInt64(UserDefaults.standard.string(forKey: key) ?? "") ?? 0 }
    public func setLastCounter(_ value: UInt64) { UserDefaults.standard.set(String(value), forKey: key) }
}

/// Builds and signs what carries the person's authority: orders (one
/// runtime, a per-device counter, an expiry) and Settings (versioned,
/// fleet-wide). Counters only rise: max(last + 1, unix milliseconds, one
/// above what the runtime last took from this device), so a reinstall that
/// kept its key never falls below what it used before.
public final class FabricCommander: Sendable {
    public let key: any FabricDeviceKey
    let counters: any FabricCounterStore
    private let lock = Mutex(())

    /// A reply from the phone lives an hour; an approval as long as its request.
    public static let replyLifetime: TimeInterval = 3600
    public static let maxTextBytes = 16 * 1024

    public init(key: any FabricDeviceKey, counters: any FabricCounterStore = MemoryCounterStore()) {
        self.key = key
        self.counters = counters
    }

    public var deviceID: String { key.deviceID }

    /// The next counter for `runtime`.
    func nextCounter(now: Date, runtimeFloor: UInt64) -> UInt64 {
        lock.withLock { _ in
            let ms = UInt64(max(0, now.timeIntervalSince1970 * 1000))
            let next = max(counters.lastCounter() + 1, ms, runtimeFloor + 1)
            counters.setLastCounter(next)
            return next
        }
    }

    /// Words that may be typed into a session.
    public static func check(text: String) throws {
        let lead = text.drop { $0.isWhitespace || $0.isNewline }
        guard !lead.isEmpty else { throw FabricOrderProblem.emptyText }
        if lead.first == "!" || lead.first == "/" { throw FabricOrderProblem.unsafeText }
        if text.utf8.count > maxTextBytes { throw FabricOrderProblem.tooLong }
    }

    /// An order, built and signed. `runtime` is the fleet's picture of the
    /// target (its devices give the counter floor).
    public func order(_ what: FabricOrder.What, runtime: FleetRuntime, now: Date = .now,
                      lifetime: TimeInterval = FabricCommander.replyLifetime, orderID: String = UUID().uuidString.lowercased()) throws -> (FabricOrder, FabricSignedBody) {
        if case .command(let c) = what, c.verb == .send || c.verb == .answer { try Self.check(text: c.text) }
        let floor = runtime.devices.first { $0.deviceID == deviceID }?.counter ?? 0
        let order = FabricOrder(runtimeID: runtime.id, counter: nextCounter(now: now, runtimeFloor: floor),
                                expiresAt: Int64(now.addingTimeInterval(lifetime).timeIntervalSince1970.rounded(.down)),
                                what: what, orderID: orderID, issuedAt: Int64(now.timeIntervalSince1970.rounded(.down)))
        let payload = order.encoded()
        return (order, FabricSignedBody(payload: payload, deviceID: deviceID, signature: try key.sign(.order, payload)))
    }

    /// The person's settings at `version`, signed.
    public func settings(_ settings: FabricSettings, version: UInt64, now: Date = .now) throws -> (FabricSettings, FabricSignedBody) {
        var s = settings
        s.version = version
        s.issuedAt = Int64(now.timeIntervalSince1970.rounded(.down))
        let payload = s.encoded()
        return (s, FabricSignedBody(payload: payload, deviceID: deviceID, signature: try key.sign(.settings, payload)))
    }
}

/// Where the fabric keeps its secrets: the Keychain (through Valet) in the
/// app, a dictionary in tests. Only a missing item reads as nil; any other
/// failure (a locked Keychain, a denied access) throws, so a key is never
/// minted, nor a trust list overwritten, because a read failed.
public protocol FabricSecretStore: Sendable {
    func data(forKey key: String) throws -> Data?
    func set(_ data: Data, forKey key: String) throws
}

/// The Keychain, this device only, readable after first unlock (a
/// notification action can sign).
public struct KeychainSecrets: FabricSecretStore, @unchecked Sendable {
    let valet: Valet

    public init(identifier: String = "ie.null.taisce.fabric") {
        valet = .valet(with: Identifier(nonEmpty: identifier)!, accessibility: .afterFirstUnlockThisDeviceOnly)
    }

    public func data(forKey key: String) throws -> Data? {
        do {
            return try valet.object(forKey: key)
        } catch KeychainError.itemNotFound {
            return nil
        }
    }

    public func set(_ data: Data, forKey key: String) throws { try valet.setObject(data, forKey: key) }
}

/// Secrets in memory (tests, previews). `failure` makes every read throw
/// it, as a locked or denied Keychain does.
public final class MemorySecrets: FabricSecretStore {
    private let items = Mutex<[String: Data]>([:])
    private let failing = Mutex<(any Error & Sendable)?>(nil)
    public init(_ start: [String: Data] = [:]) { items.withLock { $0 = start } }
    public func fail(with error: (any Error & Sendable)?) { failing.withLock { $0 = error } }
    public func data(forKey key: String) throws -> Data? {
        if let e = failing.withLock({ $0 }) { throw e }
        return items.withLock { $0[key] }
    }
    public func set(_ data: Data, forKey key: String) throws {
        if let e = failing.withLock({ $0 }) { throw e }
        items.withLock { $0[key] = data }
    }
}

/// The runtimes' own keys, as read from their pairing codes: what approval
/// and question signatures are checked against. Never taken from the hub.
public protocol FabricTrustStore: Sendable {
    /// nil when none is trusted, or the list cannot be read (nothing then verifies)
    func runtimeKey(_ runtimeID: String) -> Data?
    /// throws, and changes nothing, when the list cannot be read
    func trust(_ runtimeID: String, key: Data) throws
    func forget(_ runtimeID: String) throws
    func trusted() -> [String: Data]
    /// The whole list; throws when it cannot be read (said, never taken for "not paired")
    func load() throws -> [String: Data]
}

public final class MemoryTrustStore: FabricTrustStore {
    private let keys = Mutex<[String: Data]>([:])
    public init(_ start: [String: Data] = [:]) { keys.withLock { $0 = start } }
    public func runtimeKey(_ runtimeID: String) -> Data? { keys.withLock { $0[runtimeID.lowercased()] } }
    public func trust(_ runtimeID: String, key: Data) { keys.withLock { $0[runtimeID.lowercased()] = key } }
    public func forget(_ runtimeID: String) { _ = keys.withLock { $0.removeValue(forKey: runtimeID.lowercased()) } }
    public func trusted() -> [String: Data] { keys.withLock { $0 } }
    public func load() -> [String: Data] { trusted() }
}

/// In the Keychain (integrity matters more than secrecy: a key put here is
/// what an approval is believed against), this device only.
public struct KeychainTrustStore: FabricTrustStore {
    let secrets: any FabricSecretStore
    static let item = "runtime-keys"

    public init(secrets: any FabricSecretStore = KeychainSecrets()) {
        self.secrets = secrets
    }

    /// The list; empty only when there is none yet.
    public func load() throws -> [String: Data] {
        guard let data = try secrets.data(forKey: Self.item) else { return [:] }
        return try JSONDecoder().decode([String: Data].self, from: data)
    }

    public func trusted() -> [String: Data] { (try? load()) ?? [:] }

    public func runtimeKey(_ runtimeID: String) -> Data? { trusted()[runtimeID.lowercased()] }

    public func trust(_ runtimeID: String, key: Data) throws {
        var map = try load()
        map[runtimeID.lowercased()] = key
        try secrets.set(try JSONEncoder().encode(map), forKey: Self.item)
    }

    public func forget(_ runtimeID: String) throws {
        var map = try load()
        guard map.removeValue(forKey: runtimeID.lowercased()) != nil else { return }
        try secrets.set(try JSONEncoder().encode(map), forKey: Self.item)
    }
}

/// An attention item whose request has been verified against the runtime's
/// own key: only this is shown as "what it wants to run" and only this is
/// approved, denied or answered.
public struct VerifiedAttention: Sendable, Hashable {
    public var item: FabricAttentionItem
    /// decoded from the signed bytes
    public var request: AttentionRequest
    /// the bytes the runtime signed: their SHA-256 is the order's request_hash
    public var bytes: Data

    public var requestHash: Data { FabricSignature.requestHash(bytes) }

    /// Verify `item` against the key read from `runtime`'s pairing code.
    public static func verify(_ item: FabricAttentionItem, trust: any FabricTrustStore) throws -> VerifiedAttention {
        guard let signed = item.signed else { throw FabricOrderProblem.unverified("The request carries no signature.") }
        let keys: [String: Data]
        do {
            keys = try trust.load()
        } catch {
            throw FabricOrderProblem.unverified("This phone's list of trusted Macs can't be read (\(error.localizedDescription)).")
        }
        guard let key = keys[item.runtimeID.lowercased()] else { throw FabricOrderProblem.notPaired }
        guard FabricSignature.verify(publicKey: key, context: .attention, payload: signed.request, signature: signed.signature) else {
            throw FabricOrderProblem.unverified("The request's signature does not match your Mac's key.")
        }
        let request: AttentionRequest
        do {
            request = try AttentionRequest.decode(signed.request)
        } catch {
            throw FabricOrderProblem.unverified("The request has parts this app cannot read.")
        }
        guard request.runtimeID.lowercased() == item.runtimeID.lowercased() else { throw FabricOrderProblem.unverified("The request names another Mac.") }
        guard request.sessionID.lowercased() == item.sessionID.lowercased() else { throw FabricOrderProblem.unverified("The request names another session.") }
        guard request.requestID.lowercased() == item.id.lowercased() else { throw FabricOrderProblem.unverified("The request is not the one named.") }
        return VerifiedAttention(item: item, request: request, bytes: signed.request)
    }

    /// The order's own expiry: the request's deadline when it has one (an
    /// approval after it means nothing), else the reply default.
    public func lifetime(now: Date) -> TimeInterval {
        guard request.expiresAt > 0 else { return FabricCommander.replyLifetime }
        return max(30, min(FabricCommander.replyLifetime, Double(request.expiresAt) - now.timeIntervalSince1970))
    }

    /// What the Mac signed it as, whatever the server's item says.
    public var kind: AttentionKind { request.kind }

    /// Only a verified approval is approved.
    public func approve() throws -> FabricCommand {
        guard kind == .approval else { throw FabricOrderProblem.wrongKind }
        return FabricCommand(sessionID: request.sessionID, verb: .approve, requestID: request.requestID, requestHash: requestHash)
    }

    /// Only a verified approval is denied.
    public func deny(because reason: String = "") throws -> FabricCommand {
        guard kind == .approval else { throw FabricOrderProblem.wrongKind }
        return FabricCommand(sessionID: request.sessionID, verb: .deny, text: reason, requestID: request.requestID, requestHash: requestHash)
    }

    /// Only a verified question is answered, and only with a choice it signed.
    /// The choice's label is typed (the runtime types `text`).
    public func answer(choice: FabricChoice) throws -> FabricCommand {
        guard kind == .question else { throw FabricOrderProblem.wrongKind }
        guard let signed = request.choices.first(where: { $0.id == choice.id }), signed.label == choice.label else {
            throw FabricOrderProblem.unverified("That choice isn't one it offered.")
        }
        return FabricCommand(sessionID: request.sessionID, verb: .answer, text: signed.label, requestID: request.requestID, requestHash: requestHash, choiceID: signed.id)
    }

    public func answer(words: String) throws -> FabricCommand {
        guard kind == .question else { throw FabricOrderProblem.wrongKind }
        return FabricCommand(sessionID: request.sessionID, verb: .answer, text: words, requestID: request.requestID, requestHash: requestHash)
    }
}

/// This device's signing key for Agent Fabric: made once, in the Secure
/// Enclave where there is one, and kept for the life of the install (a
/// Keychain item for this device only). A device with no Enclave (the
/// simulator) keeps a software key in the Keychain instead, and says so.
/// Everything a paired Mac obeys from this phone is signed with it.
public final class FabricDeviceIdentity: Sendable {
    public enum Kind: Sendable, Equatable { case secureEnclave, software }

    private let state = Mutex<(key: (any FabricDeviceKey)?, kind: Kind)>((nil, .software))
    private let secrets: (any FabricSecretStore)?
    private let enclave: Bool
    static let enclaveItem = "device-key"
    static let softwareItem = "device-key-software"

    /// `enclave` nil: the Secure Enclave wherever there is one.
    public init(secrets: any FabricSecretStore = KeychainSecrets(), enclave: Bool? = nil) {
        self.secrets = secrets
        self.enclave = enclave ?? SecureEnclaveDeviceKey.isAvailable
    }

    /// A key held in memory only (tests, previews): the Keychain is never touched.
    public init(key: any FabricDeviceKey) {
        secrets = nil
        enclave = false
        state.withLock { $0 = (key, .software) }
    }

    public var kind: Kind { state.withLock { $0.kind } }

    /// The key, made the first time it is asked for. Made only when the
    /// Keychain says there is none: a read that fails throws instead.
    public func current() throws -> any FabricDeviceKey {
        try state.withLock { s in
            if let key = s.key { return key }
            guard let secrets else { throw KeychainError.itemNotFound }
            let made: (any FabricDeviceKey, Kind)
            if enclave {
                if let blob = try secrets.data(forKey: Self.enclaveItem) {
                    made = (try SecureEnclaveDeviceKey(dataRepresentation: blob), .secureEnclave)
                } else {
                    let k = try SecureEnclaveDeviceKey()
                    try secrets.set(k.dataRepresentation, forKey: Self.enclaveItem)
                    made = (k, .secureEnclave)
                }
            } else if let raw = try secrets.data(forKey: Self.softwareItem) {
                made = (SoftwareDeviceKey(try P256.Signing.PrivateKey(rawRepresentation: raw)), .software)
            } else {
                let k = SoftwareDeviceKey()
                try secrets.set(k.key.rawRepresentation, forKey: Self.softwareItem)
                made = (k, .software)
            }
            s = (made.0, made.1)
            return made.0
        }
    }

    /// The key if one has been made, without making one.
    public func existing() -> (any FabricDeviceKey)? {
        if let key = state.withLock({ $0.key }) { return key }
        guard let secrets, (try? secrets.data(forKey: enclave ? Self.enclaveItem : Self.softwareItem)) != nil else { return nil }
        return try? current()
    }
}
