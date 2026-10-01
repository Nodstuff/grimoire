import Foundation
import Testing
@testable import TaisceKit

/// Opt-in, read-only smoke against a real daemon:
/// `TAISCE_LIVE_URL=http://127.0.0.1:7425 swift test --filter LiveServer`.
/// GETs only, and never `/api/todo` (a GET for today can carry items forward).
@Suite(.enabled(if: ProcessInfo.processInfo.environment["TAISCE_LIVE_URL"] != nil))
struct LiveServerTests {
    var api: APIClient? {
        ProcessInfo.processInfo.environment["TAISCE_LIVE_URL"]
            .flatMap(URL.init(string:))
            .map { APIClient(config: ServerConfig(baseURL: $0)) }
    }

    @Test func decodesAndRendersRealDocs() async throws {
        let api = try #require(api)
        let tree = try await api.tree()
        #expect(!tree.isEmpty)
        let cache = try Cache.inMemory()
        try await cache.replaceTree(tree)
        var blocks = 0
        for summary in tree.prefix(40) {
            let doc = try await api.doc(summary.id)
            try await cache.storeDoc(doc)
            let rendered = BlockRenderer.render(try await cache.blocks(of: summary.id))
            blocks += rendered.count
        }
        let hits = try await api.search("the")
        print("live: \(tree.count) docs, \(blocks) blocks rendered from 40, \(hits.count) search hits")
    }
}

/// Opt-in, read-only discovery check against a server-mode daemon:
/// `TAISCE_LIVE_AUTH_URL=https://taisce.null.ie swift test --filter LiveAuth`.
/// Two GETs of public metadata; never registers a client or creates a grant.
@Suite(.enabled(if: ProcessInfo.processInfo.environment["TAISCE_LIVE_AUTH_URL"] != nil))
struct LiveAuthDiscoveryTests {
    @Test func decodesTheRealMetadata() async throws {
        let base = try #require(ProcessInfo.processInfo.environment["TAISCE_LIVE_AUTH_URL"].flatMap(URL.init(string:)))
        let oauth = OAuthClient(baseURL: base)
        let d = try #require(try await oauth.discover())
        #expect(OAuthClient.trimmed(d.resource.resource) == oauth.origin)
        #expect(d.server.registrationEndpoint != nil && d.server.revocationEndpoint != nil)
        #expect(d.server.tokenEndpointAuthMethodsSupported?.contains("none") == true)
        // the probe: GET /api without a token is a 401 carrying the metadata URL
        let (_, response) = try await URLSession.shared.data(from: base.appending(path: "api/docs"))
        let http = try #require(response as? HTTPURLResponse)
        #expect(http.statusCode == 401)
        #expect(http.value(forHTTPHeaderField: "WWW-Authenticate")?.contains("resource_metadata=") == true)
        print("live auth: issuer \(d.server.issuer), resource \(d.resource.resource), scopes \(d.resource.scopesSupported ?? [])")
    }
}
