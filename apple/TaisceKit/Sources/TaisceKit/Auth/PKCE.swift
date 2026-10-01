import CryptoKit
import Foundation

/// RFC 7636 proof key, S256 only (the server refuses `plain`).
public struct PKCE: Sendable, Hashable {
    public var verifier: String
    public var challenge: String { Self.challenge(for: verifier) }

    /// 32 random bytes → a 43-char base64url verifier.
    public init() {
        verifier = Self.randomURLSafe(bytes: 32)
    }

    public init(verifier: String) {
        self.verifier = verifier
    }

    /// BASE64URL(SHA256(ascii(verifier))), no padding.
    public static func challenge(for verifier: String) -> String {
        base64URL(Data(SHA256.hash(data: Data(verifier.utf8))))
    }

    public static func randomURLSafe(bytes n: Int) -> String {
        var g = SystemRandomNumberGenerator()
        return base64URL(Data((0..<n).map { _ in UInt8.random(in: .min ... .max, using: &g) }))
    }

    static func base64URL(_ d: Data) -> String {
        d.base64EncodedString()
            .replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "=", with: "")
    }
}
