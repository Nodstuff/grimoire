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
