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

/// The runtimes' own keys, as read from their pairing codes: what approval
/// and question signatures are checked against. Never taken from the hub.
public protocol FabricTrustStore: Sendable {
    func runtimeKey(_ runtimeID: String) -> Data?
    func trust(_ runtimeID: String, key: Data)
    func forget(_ runtimeID: String)
    func trusted() -> [String: Data]
}

public final class MemoryTrustStore: FabricTrustStore {
    private let keys = Mutex<[String: Data]>([:])
    public init(_ start: [String: Data] = [:]) { keys.withLock { $0 = start } }
    public func runtimeKey(_ runtimeID: String) -> Data? { keys.withLock { $0[runtimeID.lowercased()] } }
    public func trust(_ runtimeID: String, key: Data) { keys.withLock { $0[runtimeID.lowercased()] = key } }
    public func forget(_ runtimeID: String) { _ = keys.withLock { $0.removeValue(forKey: runtimeID.lowercased()) } }
    public func trusted() -> [String: Data] { keys.withLock { $0 } }
}

/// In the Keychain (integrity matters more than secrecy: a key put here is
/// what an approval is believed against), this device only.
public struct KeychainTrustStore: FabricTrustStore {
    let valet: Valet
    static let item = "runtime-keys"

    public init(identifier: String = "ie.null.taisce.fabric") {
        valet = .valet(with: Identifier(nonEmpty: identifier)!, accessibility: .afterFirstUnlockThisDeviceOnly)
    }

    public func trusted() -> [String: Data] {
        guard let data = try? valet.object(forKey: Self.item),
              let map = try? JSONDecoder().decode([String: Data].self, from: data) else { return [:] }
        return map
    }

    private func write(_ map: [String: Data]) {
        if let data = try? JSONEncoder().encode(map) { try? valet.setObject(data, forKey: Self.item) }
    }

    public func runtimeKey(_ runtimeID: String) -> Data? { trusted()[runtimeID.lowercased()] }
    public func trust(_ runtimeID: String, key: Data) {
        var map = trusted()
        map[runtimeID.lowercased()] = key
        write(map)
    }
    public func forget(_ runtimeID: String) {
        var map = trusted()
        map.removeValue(forKey: runtimeID.lowercased())
        write(map)
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
        guard let key = trust.runtimeKey(item.runtimeID) else { throw FabricOrderProblem.notPaired }
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
        return VerifiedAttention(item: item, request: request, bytes: signed.request)
    }

    /// The order's own expiry: the request's deadline when it has one (an
    /// approval after it means nothing), else the reply default.
    public func lifetime(now: Date) -> TimeInterval {
        guard request.expiresAt > 0 else { return FabricCommander.replyLifetime }
        return max(30, min(FabricCommander.replyLifetime, Double(request.expiresAt) - now.timeIntervalSince1970))
    }

    public func approve() -> FabricCommand {
        FabricCommand(sessionID: request.sessionID, verb: .approve, requestID: request.requestID, requestHash: requestHash)
    }

    public func deny(because reason: String = "") -> FabricCommand {
        FabricCommand(sessionID: request.sessionID, verb: .deny, text: reason, requestID: request.requestID, requestHash: requestHash)
    }

    /// The choice's label is typed (the runtime types `text`).
    public func answer(choice: FabricChoice) -> FabricCommand {
        FabricCommand(sessionID: request.sessionID, verb: .answer, text: choice.label, requestID: request.requestID, requestHash: requestHash, choiceID: choice.id)
    }

    public func answer(words: String) -> FabricCommand {
        FabricCommand(sessionID: request.sessionID, verb: .answer, text: words, requestID: request.requestID, requestHash: requestHash)
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
    private let valet: Valet
    static let enclaveItem = "device-key"
    static let softwareItem = "device-key-software"

    public init(identifier: String = "ie.null.taisce.fabric") {
        valet = .valet(with: Identifier(nonEmpty: identifier)!, accessibility: .afterFirstUnlockThisDeviceOnly)
    }

    public var kind: Kind { state.withLock { $0.kind } }

    /// The key, made the first time it is asked for.
    public func current() throws -> any FabricDeviceKey {
        try state.withLock { s in
            if let key = s.key { return key }
            let made: (any FabricDeviceKey, Kind)
            if SecureEnclaveDeviceKey.isAvailable {
                if let blob = try? valet.object(forKey: Self.enclaveItem), let k = try? SecureEnclaveDeviceKey(dataRepresentation: blob) {
                    made = (k, .secureEnclave)
                } else {
                    let k = try SecureEnclaveDeviceKey()
                    try valet.setObject(k.dataRepresentation, forKey: Self.enclaveItem)
                    made = (k, .secureEnclave)
                }
            } else if let raw = try? valet.object(forKey: Self.softwareItem), let k = try? P256.Signing.PrivateKey(rawRepresentation: raw) {
                made = (SoftwareDeviceKey(k), .software)
            } else {
                let k = SoftwareDeviceKey()
                try valet.setObject(k.key.rawRepresentation, forKey: Self.softwareItem)
                made = (k, .software)
            }
            s = (made.0, made.1)
            return made.0
        }
    }

    /// The key if one has been made, without making one.
    public func existing() -> (any FabricDeviceKey)? {
        if let key = state.withLock({ $0.key }) { return key }
        let item = SecureEnclaveDeviceKey.isAvailable ? Self.enclaveItem : Self.softwareItem
        guard (try? valet.containsObject(forKey: item)) == true else { return nil }
        return try? current()
    }
}
