import Foundation
import Testing
import TaisceKit

/// Runs inside the app on a simulator or device: the Keychain needs the
/// app's entitlements, which `swift test` on macOS doesn't have.
@Suite struct KeychainTests {
    let server = "https://taisce.test"

    @Test func tokensSurviveANewStoreInstance() async throws {
        let id = "ie.null.taisce.tests.\(UUID().uuidString.prefix(8))"
        let t = TokenSet(accessToken: "a", refreshToken: "r", expiresAt: Date(timeIntervalSince1970: 1_900_000_000), scope: "grimoire")
        try KeychainTokenStore(identifier: id).setTokens(t, for: server)
        try KeychainTokenStore(identifier: id).setClientID("dcr_k", for: server)
        // a relaunch: new store, new session, same Keychain
        let again = try KeychainTokenStore(identifier: id)
        #expect(try again.tokens(for: server) == t)
        #expect(try again.clientID(for: server) == "dcr_k")
        let base = try #require(URL(string: server))
        let session = AuthSession(oauth: OAuthClient(baseURL: base), store: again, now: { Date(timeIntervalSince1970: 1_800_000_000) })
        #expect(await session.state == .signedIn)
        #expect(try await session.token() == "a")
        // sign-out's clear
        try again.setTokens(nil, for: server)
        try again.setClientID(nil, for: server)
        #expect(try again.tokens(for: server) == nil)
        #expect(try again.clientID(for: server) == nil)
        #expect(throws: KeychainTokenStore.EmptyIdentifier.self) { try KeychainTokenStore(identifier: "") }
    }
}
