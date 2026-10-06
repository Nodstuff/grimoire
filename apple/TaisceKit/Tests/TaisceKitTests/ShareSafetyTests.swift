import Foundation
import Testing
@testable import TaisceKit

/// Names resolve to what the test says.
private struct FakeResolver: HostResolving {
    var table: [String: [ResolvedAddress]]
    func resolve(_ host: String) async throws -> [ResolvedAddress] {
        guard let a = table[host.lowercased()] else { throw ShareImageRejection.unresolvable }
        return a
    }
}

private let publicIP = ResolvedAddress.v4([93, 184, 216, 34])

private func loader(_ server: MockServer, resolver: FakeResolver = FakeResolver(table: ["images.example.com": [publicIP]]), maxBytes: Int = ShareLimits.assetBytes) -> SafeShareImageLoader {
    let c = URLSessionConfiguration.ephemeral
    c.protocolClasses = [MockURLProtocol.self]
    c.httpAdditionalHeaders = [MockURLProtocol.header: server.id]
    return SafeShareImageLoader(policy: ShareImagePolicy(resolver: resolver), configuration: c, maxBytes: maxBytes)
}

private let png: [UInt8] = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 13, 0x49, 0x48, 0x44, 0x52, 0, 0, 1, 0, 0, 0, 0, 0x80]

@Suite struct ShareImagePolicyTests {
    @Test func namesThatAreNeverFetched() {
        func check(_ s: String) -> ShareImageRejection? { ShareImagePolicy.check(URL(string: s)!) }
        #expect(check("http://images.example.com/a.png") == .notHTTPS)
        #expect(check("ftp://images.example.com/a.png") == .notHTTPS)
        #expect(check("https://127.0.0.1/a.png") == .ipLiteral)
        #expect(check("https://[::1]/a.png") == .ipLiteral)
        #expect(check("https://2130706433/a.png") == .ipLiteral)
        #expect(check("https://0x7f.1/a.png") == .ipLiteral)
        #expect(check("https://0177.0.0.1/a.png") == .ipLiteral)
        #expect(check("https://10.0.0.1/a.png") == .ipLiteral)
        #expect(check("https://localhost/a.png") == .localName)
        #expect(check("https://localhost./a.png") == .localName)
        #expect(check("https://api.localhost/a.png") == .localName)
        #expect(check("https://printer.local/a.png") == .localName)
        #expect(check("https://db.internal/a.png") == .localName)
        #expect(check("https://intranet/a.png") == .localName)
        #expect(check("https://user:pw@images.example.com/a.png") == .localName)
        #expect(check("https://images.example.com/a.png") == nil)
    }

    @Test func addressRanges() {
        func v4(_ s: String) -> ResolvedAddress { .v4(s.split(separator: ".").map { UInt8($0)! }) }
        for bad in ["127.0.0.1", "10.1.2.3", "172.16.0.1", "172.31.255.255", "192.168.1.1", "100.64.0.1", "100.127.255.255",
                    "169.254.169.254", "0.0.0.0", "224.0.0.1", "255.255.255.255", "198.18.0.1"] {
            #expect(!v4(bad).isPublic, "\(bad)")
        }
        for good in ["93.184.216.34", "8.8.8.8", "172.32.0.1", "100.128.0.1", "1.1.1.1"] {
            #expect(v4(good).isPublic, "\(good)")
        }
        func v6(_ s: String) -> ResolvedAddress {
            var a = in6_addr()
            inet_pton(AF_INET6, s, &a)
            return .v6(withUnsafeBytes(of: a) { Array($0) })
        }
        for bad in ["::1", "::", "fe80::1", "fc00::1", "fd12:3456::1", "ff02::1", "::ffff:127.0.0.1", "::ffff:10.0.0.1", "64:ff9b::a9fe:a9fe", "2002:7f00:1::", "2001:db8::1"] {
            #expect(!v6(bad).isPublic, "\(bad)")
        }
        for good in ["2606:4700::1111", "2a00:1450:4009::200e", "::ffff:8.8.8.8"] {
            #expect(v6(good).isPublic, "\(good)")
        }
        #expect(v6("fe80::1").description == "fe80::1")
    }

    @Test func aPublicNameThatResolvesPrivateIsRefused() async {
        let policy = ShareImagePolicy(resolver: FakeResolver(table: [
            "evil.example.com": [publicIP, .v4([127, 0, 0, 1])],
            "meta.example.com": [.v4([169, 254, 169, 254])],
            "ok.example.com": [publicIP],
        ]))
        #expect(await policy.allows(URL(string: "https://evil.example.com/a.png")!) == .privateAddress("127.0.0.1"))
        #expect(await policy.allows(URL(string: "https://meta.example.com/a.png")!) == .privateAddress("169.254.169.254"))
        #expect(await policy.allows(URL(string: "https://nowhere.example.com/a.png")!) == .unresolvable)
        #expect(await policy.allows(URL(string: "https://ok.example.com/a.png")!) == nil)
    }

    @Test func theRealResolverSeesLoopback() async throws {
        let a = try await SystemHostResolver().resolve("localhost")
        #expect(!a.isEmpty && a.allSatisfy { !$0.isPublic })
    }
}

@Suite struct SafeImageLoaderTests {
    @Test func fetchesByMagicBytesNotContentType() async throws {
        let server = MockServer { r in
            switch r.path {
            case "/a.png": MockServer.Reply(chunks: [Data(png)], contentType: "text/html")
            case "/b": MockServer.Reply(chunks: [Data("<html><body>hi</body></html>".utf8)], contentType: "image/png")
            default: MockServer.Reply(status: 404, chunks: [])
            }
        }
        let l = loader(server)
        let a = try await l.load(URL(string: "https://images.example.com/a.png")!)
        #expect(a.contentType == "image/png" && a.width == 256 && a.height == 128)
        await #expect(throws: ShareImageRejection.notAnImage) { _ = try await l.load(URL(string: "https://images.example.com/b")!) }
        await #expect(throws: ShareImageRejection.http(404)) { _ = try await l.load(URL(string: "https://images.example.com/c")!) }
        // refused before any request
        await #expect(throws: ShareImageRejection.notHTTPS) { _ = try await l.load(URL(string: "http://images.example.com/a.png")!) }
        await #expect(throws: ShareImageRejection.unresolvable) { _ = try await l.load(URL(string: "https://other.example.com/a.png")!) }
        #expect(server.requests.count == 3)
        // no cookies or credentials go out
        #expect(server.requests.allSatisfy { $0.value(forHTTPHeaderField: "Cookie") == nil && $0.value(forHTTPHeaderField: "Authorization") == nil })
    }

    @Test func sizeIsCappedByHeaderAndByStream() async throws {
        let declared = MockServer { _ in MockServer.Reply(chunks: [Data(png)], contentType: "image/png", headers: ["Content-Length": "3000000"]) }
        await #expect(throws: ShareImageRejection.tooLarge) { _ = try await loader(declared).load(URL(string: "https://images.example.com/a.png")!) }
        let big = Data(png) + Data(count: 5000)
        let streamed = MockServer { _ in MockServer.Reply(chunks: [big.prefix(2000), big.dropFirst(2000)], contentType: "image/png") }
        await #expect(throws: ShareImageRejection.tooLarge) {
            _ = try await loader(streamed, maxBytes: 1000).load(URL(string: "https://images.example.com/a.png")!)
        }
    }

    @Test func redirectsAreCheckedAndCounted() async throws {
        let server = MockServer { _ in .json("{}") }
        let resolver = FakeResolver(table: ["images.example.com": [publicIP], "internal.example.com": [.v4([10, 0, 0, 5])]])
        let l = loader(server, resolver: resolver)
        #expect(await l.allowRedirect(to: URL(string: "https://images.example.com/b.png"), hops: 1))
        #expect(!(await l.allowRedirect(to: URL(string: "https://internal.example.com/b.png"), hops: 1)), "a redirect into a private range")
        #expect(!(await l.allowRedirect(to: URL(string: "http://images.example.com/b.png"), hops: 1)))
        #expect(!(await l.allowRedirect(to: URL(string: "https://169.254.169.254/latest"), hops: 1)))
        #expect(!(await l.allowRedirect(to: URL(string: "https://images.example.com/b.png"), hops: 4)))
        // through the delegate, as URLSession calls it: the fourth hop is refused
        let task = URLSession.shared.dataTask(with: URL(string: "https://images.example.com/a.png")!)
        let resp = HTTPURLResponse(url: URL(string: "https://images.example.com/a.png")!, statusCode: 302, httpVersion: nil, headerFields: nil)!
        let next = URLRequest(url: URL(string: "https://images.example.com/b.png")!)
        for _ in 0..<3 { #expect(await l.urlSession(URLSession.shared, task: task, willPerformHTTPRedirection: resp, newRequest: next) != nil) }
        #expect(await l.urlSession(URLSession.shared, task: task, willPerformHTTPRedirection: resp, newRequest: next) == nil)
        let other = URLSession.shared.dataTask(with: URL(string: "https://images.example.com/a.png")!)
        #expect(await l.urlSession(URLSession.shared, task: other, willPerformHTTPRedirection: resp, newRequest: URLRequest(url: URL(string: "https://internal.example.com/x")!)) == nil)
    }

    @Test func authChallengesAreRefused() async {
        let server = MockServer { _ in .json("{}") }
        let l = loader(server)
        let task = URLSession.shared.dataTask(with: URL(string: "https://images.example.com/a.png")!)
        let space = URLProtectionSpace(host: "images.example.com", port: 443, protocol: "https", realm: "r", authenticationMethod: NSURLAuthenticationMethodHTTPBasic)
        let challenge = URLAuthenticationChallenge(protectionSpace: space, proposedCredential: nil, previousFailureCount: 0, failureResponse: nil, error: nil, sender: NoSender())
        let (disposition, credential) = await l.urlSession(URLSession.shared, task: task, didReceive: challenge)
        #expect(disposition == .cancelAuthenticationChallenge && credential == nil)
        #expect(l.configuration.httpCookieStorage == nil && l.configuration.urlCache == nil && l.configuration.urlCredentialStorage == nil)
    }
}

private final class NoSender: NSObject, URLAuthenticationChallengeSender {
    func use(_ credential: URLCredential, for challenge: URLAuthenticationChallenge) {}
    func continueWithoutCredential(for challenge: URLAuthenticationChallenge) {}
    func cancel(_ challenge: URLAuthenticationChallenge) {}
}

private struct SVGs: ShareVisualRendering {
    func render(_ v: ShareVisual) async throws -> RenderedVisual {
        RenderedVisual(data: Data("<svg>\(v.source)</svg>".utf8), contentType: "image/svg+xml")
    }
}

private func blocks(_ contents: [String], types: [BlockType]? = nil) -> [Block] {
    contents.enumerated().map { i, c in
        Block(id: "b\(i)", docID: "d", parentID: nil, orderKey: "\(i)", blockType: types?[i] ?? .paragraph, content: c)
    }
}

@Suite struct ShareScannerTests {
    @Test func indentedCodeOutsideListsIsLeftAlone() async {
        let md = "Intro\n\n    ```mermaid\n    graph LR\n    ```\n\n    ![x](https://images.example.com/a.png)"
        let s = await ShareSnapshotBuilder(renderer: SVGs()).build(title: "T", blocks: blocks([md]), theme: .light).snapshot
        #expect(s.assets.isEmpty)
        #expect(s.markdown == md)
        #expect(ShareSnapshotBuilder.linkedImages(title: "T", blocks: blocks([md])).isEmpty)
    }

    @Test func fencesInsideQuotesAndTabbedLists() async {
        let md = "> Note\n>\n> ```mermaid\n> graph TD\n> A-->B\n> ```\n\nafter\n\n-\titem\n\n\t```reladraw\n\tnode a\n\t```"
        let s = await ShareSnapshotBuilder(renderer: SVGs()).build(title: "T", blocks: blocks([md]), theme: .light).snapshot
        #expect(s.assets.map { String(decoding: $0.data, as: UTF8.self) } == ["<svg>graph TD\nA-->B</svg>", "<svg>node a</svg>"])
        #expect(s.markdown.contains("> ![Mermaid diagram](taisce-asset:d1.svg)\n\nafter"))
        #expect(s.markdown.contains("\t![Reladraw diagram](taisce-asset:d2.svg)"))
    }

    @Test func aQuotedFenceEndsWithItsQuote() async {
        let md = "> ```mermaid\n> graph TD\nnot quoted"
        let s = await ShareSnapshotBuilder(renderer: SVGs()).build(title: "T", blocks: blocks([md]), theme: .light).snapshot
        #expect(s.markdown == "> ![Mermaid diagram](taisce-asset:d1.svg)\nnot quoted")
    }

    @Test func imageURLsWithBalancedParensAndTitles() {
        let md = "![a](https://en.example.org/wiki/Foo_(bar).png \"T (x)\") and ![b](<https://images.example.com/a b.png>) and ![c](https://x.example.com/q?(1)(2))"
        let urls = ShareSnapshotBuilder.linkedImages(title: "T", blocks: blocks([md])).map(\.absoluteString)
        #expect(urls.count == 3)
        #expect(urls[0] == "https://en.example.org/wiki/Foo_(bar).png")
        #expect(urls[2] == "https://x.example.com/q?(1)(2)")
    }

    @Test func notFetchingKeepsImagesAsLinks() async {
        let md = "See ![logo](https://images.example.com/a.png)"
        let r = await ShareSnapshotBuilder(renderer: SVGs(), images: nil).build(title: "T", blocks: blocks([md]), theme: .light)
        #expect(r.snapshot.markdown == "See [logo](https://images.example.com/a.png)")
        #expect(r.problems.isEmpty && r.snapshot.assets.isEmpty)
    }

    @Test func refusedImagesSayWhy() async {
        let server = MockServer { _ in MockServer.Reply(chunks: [Data(png)]) }
        let md = "![a](https://images.example.com/a.png) ![b](https://127.0.0.1/b.png) ![c](http://images.example.com/c.png)"
        let r = await ShareSnapshotBuilder(renderer: SVGs(), images: loader(server)).build(title: "T", blocks: blocks([md]), theme: .light)
        #expect(r.snapshot.assets.map(\.name) == ["i1.png"])
        #expect(r.problems == [
            "Image from 127.0.0.1 kept as a link: an address instead of a name",
            "Image from images.example.com kept as a link: only https images are fetched",
        ])
    }

    @Test func onlyKnownBlockTypesArePublished() async {
        let doc = blocks(["kept", "a canvas", "future", "decided", "a comment"], types: [.paragraph, .canvasScene, .other("poll"), .decision, .comment])
        let r = await ShareSnapshotBuilder(renderer: SVGs()).build(title: "T", blocks: doc, theme: .light)
        #expect(r.snapshot.markdown == "kept\n\ndecided")
        #expect(r.problems == ["Canvases aren't shared", "A block of type \u{201C}poll\u{201D} was left out"])
    }
}

@Suite struct ShareExpiryResolveTests {
    @Test func customMustBeFiveMinutesOut() throws {
        let now = Date(timeIntervalSince1970: 1000)
        #expect(throws: ShareExpiryTooSoon.self) { _ = try ShareExpiry.custom.resolve(now: now, custom: now.addingTimeInterval(299)) }
        #expect(try ShareExpiry.custom.resolve(now: now, custom: now.addingTimeInterval(301)) == now.addingTimeInterval(301))
        #expect(try ShareExpiry.never.resolve(now: now, custom: nil) == nil)
        #expect(try ShareExpiry.hour.resolve(now: now, custom: now) == now.addingTimeInterval(3600))
    }
}

@Suite struct ShareBlocksTests {
    func cached() async throws -> Cache {
        let cache = try Cache.inMemory()
        let json = Fixture.docTree("d1", title: "Doc", epoch: 5, roots: """
        {"block":\(Fixture.block("p1", doc: "d1", content: "alpha")),"children":[]}
        """)
        try await cache.storeDoc(try JSONDecoder().decode(DocTree.self, from: Data(json.utf8)))
        let other = Fixture.docTree("d2", title: "Other", epoch: 1, roots: """
        {"block":\(Fixture.block("q1", doc: "d2", content: "beta")),"children":[]}
        """)
        try await cache.storeDoc(try JSONDecoder().decode(DocTree.self, from: Data(other.utf8)))
        return cache
    }

    @Test func onlyThisDocsPendingWritesAreFoldedIn() async throws {
        let cache = try await cached()
        #expect(try await cache.shareBlocks(for: "d1").unsent == false)
        var e2 = try #require(try await cache.editor(for: "d2"))
        try await cache.enqueue([.replaceText("q1", "beta 2")], on: &e2)
        #expect(try await cache.shareBlocks(for: "d1").unsent == false, "another doc's edit")
        var e = try #require(try await cache.editor(for: "d1"))
        let refused = try await cache.enqueue([.replaceText("p1", "refused text")], on: &e)
        try await cache.markOutbox(try #require(refused.id), state: .failed, error: "forbidden")
        let afterRefusal = try await cache.shareBlocks(for: "d1")
        #expect(afterRefusal.blocks.map(\.content) == ["alpha"] && !afterRefusal.unsent, "a refused write never ships")
        var e3 = try #require(try await cache.editor(for: "d1"))
        try await cache.enqueue([.replaceText("p1", "alpha 2")], on: &e3)
        let pending = try await cache.shareBlocks(for: "d1")
        #expect(pending.blocks.map(\.content) == ["alpha 2"] && pending.unsent)
    }
}
