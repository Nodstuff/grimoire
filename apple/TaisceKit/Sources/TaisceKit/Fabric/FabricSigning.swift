import CryptoKit
import Foundation

// SIGNING in `proto/taisce/runtime/v1/runtime.proto`: ECDSA P-256 over
// SHA-256, over CONTEXT || payload, the payload carried verbatim. A
// signature is 64 bytes r || s; a public key 65 bytes 0x04 || X || Y.

/// The label each signed kind is prefixed with, so a signature for one
/// kind cannot pass as another.
public enum SigningContext: String, Sendable, CaseIterable {
    case order = "taisce/order/v1\n"
    case settings = "taisce/settings/v1\n"
    case attention = "taisce/attention/v1\n"
    case pairing = "taisce/pairing/v1\n"

    public func input(_ payload: Data) -> Data {
        Data(rawValue.utf8) + payload
    }
}

/// A key that signs for this device: the Secure Enclave's on a phone, a
/// software key in tests and on a simulator without one.
public protocol FabricDeviceKey: Sendable {
    /// 65 bytes, X9.63
    var publicKey: Data { get }
    /// 64 bytes r || s over SHA-256 of `message`
    func signature(for message: Data) throws -> Data
}

extension FabricDeviceKey {
    public var deviceID: String { FabricSignature.deviceID(publicKey: publicKey) }
    public var fingerprint: String { FabricSignature.fingerprint(deviceID: deviceID) }

    public func sign(_ context: SigningContext, _ payload: Data) throws -> Data {
        try signature(for: context.input(payload))
    }
}

/// A P-256 key held in memory.
public struct SoftwareDeviceKey: FabricDeviceKey {
    public let key: P256.Signing.PrivateKey

    public init(_ key: P256.Signing.PrivateKey = P256.Signing.PrivateKey()) {
        self.key = key
    }

    public var publicKey: Data { key.publicKey.x963Representation }

    public func signature(for message: Data) throws -> Data {
        try key.signature(for: message).rawRepresentation
    }
}

/// A key that never leaves this device's Secure Enclave. Its
/// `dataRepresentation` is an opaque blob only this Enclave can use.
public struct SecureEnclaveDeviceKey: FabricDeviceKey {
    let key: SecureEnclave.P256.Signing.PrivateKey

    public static var isAvailable: Bool { SecureEnclave.isAvailable }

    /// A new key, usable whenever the device has been unlocked once since
    /// boot (a notification action signs in the background).
    public init() throws {
        var error: Unmanaged<CFError>?
        guard let access = SecAccessControlCreateWithFlags(nil, kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly, .privateKeyUsage, &error) else {
            throw error.map { $0.takeRetainedValue() as any Error } ?? FabricSigningError.enclave
        }
        key = try SecureEnclave.P256.Signing.PrivateKey(accessControl: access)
    }

    public init(dataRepresentation: Data) throws {
        key = try SecureEnclave.P256.Signing.PrivateKey(dataRepresentation: dataRepresentation)
    }

    public var dataRepresentation: Data { key.dataRepresentation }
    public var publicKey: Data { key.publicKey.x963Representation }

    public func signature(for message: Data) throws -> Data {
        try key.signature(for: message).rawRepresentation
    }
}

public enum FabricSigningError: Error, Equatable, Sendable {
    case enclave
    case badPublicKey
}

public enum FabricSignature {
    /// lowercase hex SHA-256 of the 65-byte public key
    public static func deviceID(publicKey: Data) -> String {
        SHA256.hash(data: publicKey).hex
    }

    /// The first 16 hex characters of the device id, in fours: "3f2a 9c1d 77e0 b415".
    public static func fingerprint(deviceID: String) -> String {
        let head = Array(deviceID.prefix(16))
        return stride(from: 0, to: head.count, by: 4).map { String(head[$0..<min($0 + 4, head.count)]) }.joined(separator: " ")
    }

    /// `Command.request_hash`: SHA-256 of the AttentionRequest bytes shown.
    public static func requestHash(_ request: Data) -> Data {
        Data(SHA256.hash(data: request))
    }

    /// The signature over CONTEXT || payload, as received; either s is
    /// accepted (nothing keys on a signature's bytes).
    public static func verify(publicKey: Data, context: SigningContext, payload: Data, signature: Data) -> Bool {
        guard signature.count == 64, let key = try? P256.Signing.PublicKey(x963Representation: publicKey) else { return false }
        let message = context.input(payload)
        for candidate in [signature, lowS(signature)] {
            if let sig = try? P256.Signing.ECDSASignature(rawRepresentation: candidate), key.isValidSignature(sig, for: message) {
                return true
            }
        }
        return false
    }

    /// The group order of P-256.
    static let order: [UInt8] = Hex.bytes("ffffffff00000000ffffffffffffffffbce6faada7179e84f3b9cac2fc632551")

    /// r || (n - s): the other of the two valid s values. A verifier that
    /// takes only one of them still accepts a signature made with the other.
    static func lowS(_ signature: Data) -> Data {
        let bytes = Array(signature)
        let s = Array(bytes[32..<64])
        var out = [UInt8](repeating: 0, count: 32)
        var borrow = 0
        for i in stride(from: 31, through: 0, by: -1) {
            var d = Int(order[i]) - Int(s[i]) - borrow
            borrow = d < 0 ? 1 : 0
            if d < 0 { d += 256 }
            out[i] = UInt8(d)
        }
        return Data(bytes[0..<32] + out)
    }
}

/// A runtime's pairing code (`taisce-pair:?v=1&r=…&p=…&k=…&s=…&h=…`), as
/// the Mac shows it.
public struct PairingCode: Sendable, Hashable {
    public var runtimeID: String
    public var pairingID: String
    /// the runtime's own signing key: what approvals and questions are
    /// checked against, read here and never from the hub
    public var runtimeKey: Data
    public var secret: Data
    /// the hub the runtime reports to
    public var host: String

    public init(runtimeID: String, pairingID: String, runtimeKey: Data, secret: Data, host: String) {
        self.runtimeID = runtimeID
        self.pairingID = pairingID
        self.runtimeKey = runtimeKey
        self.secret = secret
        self.host = host
    }

    public enum ParseError: Error, Equatable, Sendable {
        case notAPairingCode
        case unsupportedVersion
        case missing(String)
        case malformed(String)
    }

    public init(_ text: String) throws {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard trimmed.hasPrefix("taisce-pair:"), let q = trimmed.firstIndex(of: "?") else { throw ParseError.notAPairingCode }
        var items: [String: String] = [:]
        for pair in trimmed[trimmed.index(after: q)...].split(separator: "&") {
            let kv = pair.split(separator: "=", maxSplits: 1).map(String.init)
            guard kv.count == 2 else { continue }
            items[kv[0]] = kv[1].removingPercentEncoding ?? kv[1]
        }
        guard items["v"] == "1" else { throw ParseError.unsupportedVersion }
        func value(_ k: String) throws -> String {
            guard let v = items[k], !v.isEmpty else { throw ParseError.missing(k) }
            return v
        }
        runtimeID = try value("r").lowercased()
        pairingID = try value("p").lowercased()
        guard UUID(uuidString: runtimeID) != nil else { throw ParseError.malformed("r") }
        guard UUID(uuidString: pairingID) != nil else { throw ParseError.malformed("p") }
        guard let k = Data(base64URL: try value("k")), k.count == 65, k.first == 0x04,
              (try? P256.Signing.PublicKey(x963Representation: k)) != nil
        else { throw ParseError.malformed("k") }
        runtimeKey = k
        guard let s = Data(base64URL: try value("s")), s.count == 32 else { throw ParseError.malformed("s") }
        secret = s
        host = try value("h")
    }

    /// The fingerprint of the runtime's key, as the Mac would show it.
    public var runtimeFingerprint: String {
        FabricSignature.fingerprint(deviceID: FabricSignature.deviceID(publicKey: runtimeKey))
    }

    /// HKDF-SHA256(secret, salt: the pairing id, info: "taisce/pairing/v1"), 32 bytes.
    public var sealKey: SymmetricKey {
        HKDF<SHA256>.deriveKey(inputKeyMaterial: SymmetricKey(data: secret), salt: Data(pairingID.utf8),
                               info: Data("taisce/pairing/v1".utf8), outputByteCount: 32)
    }

    /// The signed claim for `key`, sealed for the runtime: nonce ||
    /// ciphertext || tag, the runtime id as additional data.
    public func seal(with key: any FabricDeviceKey, deviceName: String, now: Date = .now, nonce: AES.GCM.Nonce = AES.GCM.Nonce()) throws -> Data {
        let claim = PairingClaim(runtimeID: runtimeID, pairingID: pairingID, publicKey: key.publicKey,
                                 deviceName: deviceName.truncatedUTF8(100), createdAt: Int64(now.timeIntervalSince1970))
        let payload = claim.encoded()
        let seal = PairingSeal(claim: payload, signature: try key.sign(.pairing, payload))
        let box = try AES.GCM.seal(seal.encoded(), using: sealKey, nonce: nonce, authenticating: Data(runtimeID.utf8))
        guard let combined = box.combined else { throw FabricSigningError.enclave }
        return combined
    }
}

extension String {
    /// At most `limit` bytes of UTF-8, cut at a character boundary.
    public func truncatedUTF8(_ limit: Int) -> String {
        guard utf8.count > limit else { return self }
        var out = ""
        var used = 0
        for ch in self {
            let n = String(ch).utf8.count
            guard used + n <= limit else { break }
            out.append(ch)
            used += n
        }
        return out
    }
}

extension Data {
    /// base64url without padding (the pairing code's).
    public init?(base64URL: String) {
        var s = base64URL.replacingOccurrences(of: "-", with: "+").replacingOccurrences(of: "_", with: "/")
        while s.count % 4 != 0 { s += "=" }
        self.init(base64Encoded: s)
    }

    var hex: String { map { String(format: "%02x", $0) }.joined() }
}

enum Hex {
    static func bytes(_ hex: String) -> [UInt8] {
        var out: [UInt8] = []
        var i = hex.startIndex
        while i < hex.endIndex, let j = hex.index(i, offsetBy: 2, limitedBy: hex.endIndex) {
            out.append(UInt8(hex[i..<j], radix: 16) ?? 0)
            i = j
        }
        return out
    }
}

extension SHA256.Digest {
    var hex: String { map { String(format: "%02x", $0) }.joined() }
}
