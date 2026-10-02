import Foundation
import Synchronization
import Valet

/// Per-server credentials: the registered client id (kept across sign-outs,
/// so a sign-in doesn't register a new client each time) and the tokens.
/// `server` is the origin without a trailing slash (`OAuthClient.origin`).
public protocol TokenStore: Sendable {
    func tokens(for server: String) throws -> TokenSet?
    func setTokens(_ tokens: TokenSet?, for server: String) throws
    func clientID(for server: String) throws -> String?
    func setClientID(_ id: String?, for server: String) throws
}

/// The Keychain, through Valet. `afterFirstUnlockThisDeviceOnly`: readable
/// by background refresh once the device has been unlocked, never synced or
/// restored to another device.
public struct KeychainTokenStore: TokenStore {
    let valet: Valet

    public struct EmptyIdentifier: Error {}

    public init(identifier: String = "ie.null.taisce.auth") throws {
        guard let id = Identifier(nonEmpty: identifier) else { throw EmptyIdentifier() }
        valet = .valet(with: id, accessibility: .afterFirstUnlockThisDeviceOnly)
    }

    public func tokens(for server: String) throws -> TokenSet? {
        guard let data = try read("tokens|\(server)") else { return nil }
        return try JSONDecoder().decode(TokenSet.self, from: data)
    }

    public func setTokens(_ tokens: TokenSet?, for server: String) throws {
        try write(tokens.map { try JSONEncoder().encode($0) }, "tokens|\(server)")
    }

    public func clientID(for server: String) throws -> String? {
        try read("client|\(server)").flatMap { String(data: $0, encoding: .utf8) }
    }

    public func setClientID(_ id: String?, for server: String) throws {
        try write(id.map { Data($0.utf8) }, "client|\(server)")
    }

    private func read(_ key: String) throws -> Data? {
        do {
            return try valet.object(forKey: key)
        } catch .itemNotFound {
            return nil
        }
    }

    private func write(_ data: Data?, _ key: String) throws {
        if let data {
            try valet.setObject(data, forKey: key)
        } else {
            do { try valet.removeObject(forKey: key) } catch .itemNotFound {}
        }
    }
}

/// In memory: tests, previews, and loopback servers that need no auth.
public final class MemoryTokenStore: TokenStore {
    private let state = Mutex<(tokens: [String: TokenSet], clients: [String: String])>(([:], [:]))

    public init(tokens: [String: TokenSet] = [:], clients: [String: String] = [:]) {
        state.withLock { $0 = (tokens, clients) }
    }

    public func tokens(for server: String) throws -> TokenSet? { state.withLock { $0.tokens[server] } }
    public func setTokens(_ tokens: TokenSet?, for server: String) throws { state.withLock { $0.tokens[server] = tokens } }
    public func clientID(for server: String) throws -> String? { state.withLock { $0.clients[server] } }
    public func setClientID(_ id: String?, for server: String) throws { state.withLock { $0.clients[server] = id } }
}
