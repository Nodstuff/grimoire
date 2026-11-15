import Foundation
import Synchronization
import Testing
@testable import TaisceKit

private let shareJSON = """
{"id":"s1","doc_id":"d1","url":"https://taisce.null.ie/s/tok","created_at":"2026-10-06T09:00:00Z","updated_at":"2026-10-06T09:00:00.123Z",
 "expires_at":"2026-10-13T09:00:00Z","revoked_at":null,"comments_enabled":true,"revision":2,
 "views":5,"last_viewed_at":null,"comment_count":3,"unread_comments":1,"title":"Plan"}
"""

private func json(_ r: URLRequest) -> [String: Any] {
    (try? JSONSerialization.jsonObject(with: r.httpBody ?? Data()) as? [String: Any]) ?? [:]
}

@Suite struct ShareAPITests {
    @Test func createSendsDocSnapshotExpiryAndComments() async throws {
        let server = MockServer { r in
            #expect(r.httpMethod == "POST" && r.path == "/api/shares")
            return MockServer.Reply(status: 201, chunks: [Data(shareJSON.utf8)])
        }
        let snap = ShareSnapshot(title: "Plan", markdown: "hi", assets: [ShareAsset(name: "d1.svg", contentType: "image/svg+xml", data: Data("<svg/>".utf8), width: 10, height: 5)], theme: .dark)
        let expiry = Date(timeIntervalSince1970: 1_791_000_000)
        let share = try await server.client().createShare(docID: "d1", snapshot: snap, expiresAt: expiry, commentsEnabled: false)
        #expect(share.id == "s1" && share.revision == 2 && share.views == 5 && share.unreadComments == 1)
        #expect(share.updatedAt != nil && share.expiresAt == ShareDate.parse("2026-10-13T09:00:00Z"))
        let body = json(server.requests[0])
        #expect(body["doc_id"] as? String == "d1")
        #expect(body["comments_enabled"] as? Bool == false)
        #expect(body["expires_at"] as? String == ShareDate.string(expiry))
        let s = try #require(body["snapshot"] as? [String: Any])
        #expect(s["theme"] as? String == "dark" && s["markdown"] as? String == "hi")
        let a = try #require((s["assets"] as? [[String: Any]])?.first)
        #expect(a["content_type"] as? String == "image/svg+xml")
        #expect(a["data"] as? String == Data("<svg/>".utf8).base64EncodedString())
        #expect(a["width"] as? Int == 10)
    }

    @Test func createNeverSendsNullExpiry() async throws {
        let server = MockServer { _ in MockServer.Reply(status: 201, chunks: [Data(shareJSON.utf8)]) }
        _ = try await server.client().createShare(docID: "d1", snapshot: ShareSnapshot(title: "t", markdown: "", theme: .light), expiresAt: nil, commentsEnabled: true)
        let body = json(server.requests[0])
        #expect(body.keys.contains("expires_at") && body["expires_at"] is NSNull)
    }

    @Test func listFiltersByDoc() async throws {
        let server = MockServer { _ in .json("{\"shares\":[\(shareJSON)]}") }
        let list = try await server.client().shares(docID: "d1")
        #expect(list.map(\.id) == ["s1"])
        #expect(server.requests[0].query == ["doc_id": "d1"])
        _ = try await server.client().shares(docID: nil)
    }

    @Test func patchSendsOnlyWhatChanges() async throws {
        let server = MockServer { r in
            #expect(r.httpMethod == "PATCH" && r.path == "/api/shares/s1")
            return .json(shareJSON)
        }
        let c = server.client()
        _ = try await c.updateShare("s1", commentsEnabled: false)
        _ = try await c.updateShare("s1", expiresAt: .set(nil))
        _ = try await c.updateShare("s1", snapshot: ShareSnapshot(title: "t", markdown: "m", theme: .auto))
        let bodies = server.requests.map(json)
        #expect(Set(bodies[0].keys) == ["comments_enabled"])
        #expect(Set(bodies[1].keys) == ["expires_at"] && bodies[1]["expires_at"] is NSNull)
        #expect(Set(bodies[2].keys) == ["snapshot"])
    }

    @Test func revokeAndDeleteCommentAccept204() async throws {
        let server = MockServer { r in
            #expect(r.httpMethod == "DELETE")
            return MockServer.Reply(status: 204, chunks: [], contentType: "")
        }
        try await server.client().revokeShare("s1")
        try await server.client().deleteShareComment("s1", commentID: "c1")
        #expect(server.requests.map(\.path) == ["/api/shares/s1", "/api/shares/s1/comments/c1"])
    }

    @Test func refusalsCarryTheServersMessage() async throws {
        func refusing(_ status: Int, _ body: String) -> MockServer {
            MockServer { _ in MockServer.Reply(status: status, chunks: [Data(body.utf8)]) }
        }
        let viewer = refusing(403, #"{"error":"only the owner or an editor can share this doc"}"#)
        await #expect(throws: ShareAPIError(status: 403, message: "only the owner or an editor can share this doc")) {
            _ = try await viewer.client().createShare(docID: "d1", snapshot: ShareSnapshot(title: "t", markdown: "", theme: .light), expiresAt: nil, commentsEnabled: true)
        }
        // a per-user cap is a 429 with words, not a silent "retry later"
        let capped = refusing(429, #"{"error":"you have 100 live links; revoke some first"}"#)
        do {
            _ = try await capped.client().updateShare("s1", commentsEnabled: true)
            Issue.record("expected a refusal")
        } catch let e as ShareAPIError {
            #expect(e.isLimit && e.localizedDescription == "you have 100 live links; revoke some first")
        }
        let big = refusing(413, #"{"error":"snapshots are 10 MB at most"}"#)
        await #expect(throws: ShareAPIError(status: 413, message: "snapshots are 10 MB at most")) {
            _ = try await big.client().sharePreviewHTML(ShareSnapshot(title: "t", markdown: "", theme: .light))
        }
        // no message: still something a person can read
        let bare = MockServer { _ in MockServer.Reply(status: 429, chunks: [], contentType: "text/plain") }
        await #expect(throws: ShareAPIError(status: 429, message: nil)) { try await bare.client().revokeShare("s1") }
        #expect(ShareAPIError(status: 403, message: nil).localizedDescription == "Only the doc's owner or an editor can share it.")
        let missing = refusing(404, #"{"error":"not found"}"#)
        await #expect(throws: ShareAPIError(status: 404, message: "not found")) { _ = try await missing.client().shares(docID: nil) }
        let unauthorized = refusing(401, #"{"error":"no"}"#)
        await #expect(throws: APIError.unauthorized) { _ = try await unauthorized.client().shares(docID: nil) }
    }

    @Test func markReadPostsToItsOwnRoute() async throws {
        let server = MockServer { r in
            #expect(r.httpMethod == "POST" && r.path == "/api/shares/s1/comments/read")
            return MockServer.Reply(status: 204, chunks: [], contentType: "")
        }
        try await server.client().markShareCommentsRead("s1")
        #expect(server.requests.count == 1)
    }

    @Test func previewReturnsHTML() async throws {
        let server = MockServer { r in
            #expect(r.path == "/api/shares/preview")
            #expect(json(r)["snapshot"] != nil)
            return .json(#"{"html":"<!doctype html><p>x</p>"}"#)
        }
        let html = try await server.client().sharePreviewHTML(ShareSnapshot(title: "t", markdown: "x", theme: .light))
        #expect(html.hasPrefix("<!doctype html>"))
    }

    @Test func commentsDecodeAndReply() async throws {
        let server = MockServer { r in
            if r.httpMethod == "POST" {
                let b = json(r)
                #expect(b["body"] as? String == "Thanks" && b["parent_id"] as? String == "c1")
                return MockServer.Reply(status: 201, chunks: [Data(#"{"id":"c2","parent_id":"c1","author":"Tom","is_owner":true,"body":"Thanks","anchor":null,"created_at":"2026-10-06T10:00:00Z","revision":1}"#.utf8)])
            }
            return .json(#"{"comments":[{"id":"c1","parent_id":null,"author":"Aoife","is_owner":false,"body":"Nice","anchor":{"block":3,"quote":"selected text"},"created_at":"2026-10-06T09:30:00Z","revision":1}]}"#)
        }
        let list = try await server.client().shareComments("s1")
        #expect(list.first?.anchor == ShareAnchor(block: 3, quote: "selected text"))
        #expect(list.first?.author == "Aoife" && list.first?.isOwner == false)
        let reply = try await server.client().replyToShareComment("s1", body: "Thanks", parentID: "c1")
        #expect(reply.isOwner && reply.parentID == "c1")
        #expect(server.requests.map(\.path) == ["/api/shares/s1/comments", "/api/shares/s1/comments"])
    }
}

@Suite struct ShareModelTests {
    @Test func state() throws {
        let now = Date(timeIntervalSince1970: 1_000_000)
        var s = Share(id: "s", docID: "d", url: "u")
        #expect(s.state(now: now) == .active)
        s.expiresAt = now.addingTimeInterval(-1)
        #expect(s.state(now: now) == .expired)
        s.revokedAt = now
        #expect(s.state(now: now) == .revoked)
    }

    @Test func expiryDates() {
        let now = Date(timeIntervalSince1970: 0)
        #expect(ShareExpiry.default == .week)
        #expect(ShareExpiry.hour.date(from: now) == Date(timeIntervalSince1970: 3600))
        #expect(ShareExpiry.month.date(from: now) == Date(timeIntervalSince1970: 30 * 86400))
        #expect(ShareExpiry.never.date(from: now) == nil)
        let custom = Date(timeIntervalSince1970: 99)
        #expect(ShareExpiry.custom.date(from: now, custom: custom) == custom)
        #expect(ShareExpiry.allCases.map(\.title) == ["1 hour", "1 day", "7 days", "30 days", "Custom date", "Never"])
    }

    @Test func threadsGroupRepliesUnderTheirRoot() {
        func c(_ id: String, _ parent: String?, _ t: Double) -> ShareComment {
            ShareComment(id: id, parentID: parent, author: id, body: id, createdAt: Date(timeIntervalSince1970: t))
        }
        let threads = ShareThread.group([c("a", nil, 1), c("b", nil, 2), c("a2", "a", 5), c("a1", "a", 3), c("a1x", "a1", 4), c("orphan", "gone", 6)])
        #expect(threads.map(\.id) == ["a", "b", "orphan"])
        #expect(threads[0].replies.map(\.id) == ["a1", "a1x", "a2"])
    }

    @Test func pagesAreSentOnlyWhenThereAreAny() throws {
        func object(_ s: ShareSnapshot) throws -> [String: Any] {
            try #require(try JSONSerialization.jsonObject(with: JSONEncoder().encode(s)) as? [String: Any])
        }
        let single = try object(ShareSnapshot(title: "Plan", markdown: "hi", theme: .light))
        #expect(Set(single.keys) == ["title", "markdown", "assets", "theme"], "a single-doc request is what it always was")
        let tree = ShareSnapshot(title: "Plan", markdown: "hi", theme: .light, pages: [
            SharePage(docID: "c1", parentDocID: "root", title: "Fleet Twin", markdown: "one"),
            SharePage(docID: "g1", parentDocID: "c1", title: "Deep", markdown: "two"),
            SharePage(docID: "c2", parentDocID: nil, title: "Last", markdown: "three"),
        ])
        let pages = try #require(try object(tree)["pages"] as? [[String: Any]])
        #expect(pages.map { $0["doc_id"] as? String } == ["c1", "g1", "c2"], "in the order given")
        #expect(Set(pages[0].keys) == ["doc_id", "parent_doc_id", "title", "markdown"])
        #expect(pages[0]["parent_doc_id"] as? String == "root" && pages[1]["title"] as? String == "Deep" && pages[1]["markdown"] as? String == "two")
        #expect(pages[2].keys.contains("parent_doc_id") && pages[2]["parent_doc_id"] is NSNull)
        #expect(try JSONDecoder().decode(ShareSnapshot.self, from: JSONEncoder().encode(tree)) == tree)
        let old = try JSONDecoder().decode(ShareSnapshot.self, from: Data(#"{"title":"t","markdown":"m","assets":[],"theme":"auto"}"#.utf8))
        #expect(old.pages.isEmpty)
        #expect(tree.approximateSize == "hi".utf8.count + "one".utf8.count + "two".utf8.count + "three".utf8.count)
    }

    @Test func aShareSaysHowManySubPagesItCarries() throws {
        let single = try JSONDecoder().decode(Share.self, from: Data(shareJSON.utf8))
        #expect(single.pageCount == 0, "absent = the doc alone")
        let tree = try JSONDecoder().decode(Share.self, from: Data(shareJSON.replacingOccurrences(of: #""title":"Plan""#, with: #""title":"Plan","pages":8"#).utf8))
        #expect(tree.pageCount == 8)
        #expect(try JSONDecoder().decode(Share.self, from: JSONEncoder().encode(tree)).pageCount == 8)
        #expect(Share(id: "s", docID: "d", url: "u").pageCount == 0)
        #expect(ShareLimits.assets == 1000 && ShareLimits.pages == 200)
    }

    @Test func aCommentSaysWhichPageItIsOn() throws {
        let list = try JSONDecoder().decode([ShareComment].self, from: Data(#"""
        [{"id":"c1","author":"Aoife","body":"root","page":null,"page_title":null},
         {"id":"c2","author":"Aoife","body":"sub","page":"fleet-twin","page_title":"Fleet Twin"},
         {"id":"c3","author":"Aoife","body":"gone","page":"old-page","page_title":null},
         {"id":"c4","author":"Aoife","body":"an older server"}]
        """#.utf8))
        #expect(list.map(\.page) == [nil, "fleet-twin", "old-page", nil])
        #expect(list.map(\.pageTitle) == [nil, "Fleet Twin", nil, nil])
        #expect(list.map(\.pageCaption) == [nil, "on Fleet Twin", "on a removed page", nil])
        #expect(try JSONDecoder().decode(ShareComment.self, from: JSONEncoder().encode(list[1])) == list[1])
    }

    @Test func assetNames() {
        #expect(ShareAsset.isValidName("d1.svg"))
        #expect(!ShareAsset.isValidName("../x"))
        #expect(!ShareAsset.isValidName(String(repeating: "a", count: 65)))
    }
}

/// Draws by script: per kind, an answer, an error, or a hang.
private final class FakeVisuals: ShareVisualRendering {
    enum Behaviour: Sendable { case svg, png, fail(String), hang }
    let behaviour: @Sendable (ShareVisual) -> Behaviour
    let seen = Mutex<[ShareVisual]>([])

    init(_ behaviour: @escaping @Sendable (ShareVisual) -> Behaviour) { self.behaviour = behaviour }

    func render(_ v: ShareVisual) async throws -> RenderedVisual {
        seen.withLock { $0.append(v) }
        switch behaviour(v) {
        case .svg: return RenderedVisual(data: Data("<svg>\(v.source)</svg>".utf8), contentType: "image/svg+xml", width: 300, height: 100)
        case .png: return RenderedVisual(data: Data([0x89, 0x50, 0x4E, 0x47]), contentType: "image/png", width: 640, height: 320, alt: "Sales")
        case let .fail(m): throw DiagramRenderError(message: m)
        case .hang:
            try await Task.sleep(for: .seconds(30))
            return RenderedVisual(data: Data(), contentType: "image/png")
        }
    }
}

private struct FakeImages: ShareImageLoading {
    func load(_ url: URL) async throws -> RenderedVisual {
        if url.host() == "ok.example" { return RenderedVisual(data: Data([0xFF, 0xD8, 0xFF, 0x00]), contentType: "image/jpeg") }
        throw ShareImageRejection.http(503)
    }
}

private func blocks(_ contents: [String], types: [BlockType]? = nil) -> [Block] {
    contents.enumerated().map { i, c in
        Block(id: "b\(i)", docID: "d", parentID: nil, orderKey: "\(i)", blockType: types?[i] ?? .paragraph, content: c)
    }
}

@Suite struct ShareSnapshotBuilderTests {
    @Test func visualsBecomeAssetsAndTheRestIsKept() async throws {
        let r = FakeVisuals { $0.kind == .vegaLite ? .png : .svg }
        let doc = blocks([
            "---\ntags: [a]\n---",
            "# Plan",
            "Intro **bold**",
            "```mermaid\ngraph TD; A-->B\n```",
            "```reladraw\nbox a\n```",
            "```vega-lite\n{\"mark\":\"bar\"}\n```",
            "```go\nfmt.Println(1)\n```",
            "```mermaid\nsequenceDiagram\n```",
        ], types: [.code, .heading, .paragraph, .code, .code, .code, .code, .diagramMermaid])
        let result = await ShareSnapshotBuilder(renderer: r).build(title: "Plan", blocks: doc, theme: .dark)
        let s = result.snapshot
        #expect(result.problems.isEmpty)
        #expect(s.title == "Plan" && s.theme == .dark)
        #expect(s.assets.map(\.name) == ["d1.svg", "d2.svg", "d3.png", "d4.svg"])
        #expect(s.markdown == """
        Intro **bold**

        ![Mermaid diagram](taisce-asset:d1.svg)

        ![Reladraw diagram](taisce-asset:d2.svg)

        ![Chart: Sales](taisce-asset:d3.png)

        ```go
        fmt.Println(1)
        ```

        ![Mermaid diagram](taisce-asset:d4.svg)
        """)
        #expect(String(decoding: s.assets[0].data, as: UTF8.self) == "<svg>graph TD; A-->B</svg>")
        #expect(String(decoding: s.assets[3].data, as: UTF8.self) == "<svg>sequenceDiagram</svg>")
        #expect(r.seen.withLock { $0.allSatisfy { $0.theme == .dark } })
    }

    @Test func aFailedOrSlowDiagramBecomesAnErrorCard() async throws {
        let r = FakeVisuals { $0.source.contains("slow") ? .hang : .fail("line 2: bad arrow") }
        let doc = blocks(["```mermaid\nbroken\n```", "```reladraw\nslow\n```", "after"])
        let result = await ShareSnapshotBuilder(renderer: r, timeout: .milliseconds(200)).build(title: "T", blocks: doc, theme: .light)
        let s = result.snapshot
        #expect(s.assets.map(\.name) == ["d1-error.svg", "d2-error.svg"])
        #expect(s.assets.allSatisfy { $0.contentType == "image/svg+xml" })
        let card = String(decoding: s.assets[0].data, as: UTF8.self)
        #expect(card.contains("Mermaid diagram couldn") && card.contains("line 2: bad arrow"))
        #expect(s.markdown.contains("![Mermaid diagram couldn't render: line 2: bad arrow](taisce-asset:d1-error.svg)"))
        #expect(s.markdown.hasSuffix("after"))
        #expect(result.problems.count == 2)
        #expect(result.problems[1].contains("longer than"))
    }

    @Test func errorCardEscapesXML() {
        let card = ShareSnapshotBuilder.errorCard(title: "t", detail: "a < b & \"c\"", theme: .dark)
        let s = String(decoding: card.data, as: UTF8.self)
        #expect(s.contains("a &lt; b &amp; &quot;c&quot;"))
        #expect(!s.contains("a < b"))
    }

    @Test func linkedImagesAreFetchedOrBecomeLinks() async throws {
        let r = FakeVisuals { _ in .svg }
        let doc = blocks(["See ![logo](https://ok.example/a.jpg \"Logo\") and ![x](https://down.example/b.png), `![code](https://ok.example/c.png)` and ![local](a.png)"])
        let result = await ShareSnapshotBuilder(renderer: r, images: FakeImages()).build(title: "T", blocks: doc, theme: .light)
        let s = result.snapshot
        #expect(s.assets.map(\.name) == ["i1.jpg"])
        #expect(s.markdown == "See ![logo](taisce-asset:i1.jpg) and [x](https://down.example/b.png), `![code](https://ok.example/c.png)` and ![local](a.png)")
        #expect(result.problems == ["Image from down.example kept as a link: the server answered 503"])
    }

    @Test func fencesInsideListsAndTildesAndUnclosed() async throws {
        let r = FakeVisuals { _ in .svg }
        let doc = blocks(["- item\n\n  ```mermaid\n  graph LR\n  ```\n- next", "~~~\nnot a diagram\n~~~", "```mermaid\nnever closed"])
        let s = await ShareSnapshotBuilder(renderer: r).build(title: "T", blocks: doc, theme: .light).snapshot
        #expect(s.markdown.contains("  ![Mermaid diagram](taisce-asset:d1.svg)\n- next"))
        #expect(String(decoding: s.assets[0].data, as: UTF8.self) == "<svg>graph LR</svg>")
        #expect(s.markdown.contains("~~~\nnot a diagram\n~~~"))
        #expect(s.assets.count == 2)
    }

    @Test func commentsDeletedAndTitleOnlyH1() async throws {
        let r = FakeVisuals { _ in .svg }
        var doc = blocks(["# Other", "kept", "gone", "a comment"], types: [.heading, .paragraph, .paragraph, .comment])
        doc[2].deleted = true
        let s = await ShareSnapshotBuilder(renderer: r).build(title: "Title", blocks: doc, theme: .auto).snapshot
        #expect(s.markdown == "# Other\n\nkept")
        #expect(s.theme == .auto)
    }

    @Test func frontmatterInsideTheFirstBlock() {
        #expect(ShareSnapshotBuilder.strippingFrontmatter("---\na: 1\n---\n\n# Hi") == "# Hi")
        #expect(ShareSnapshotBuilder.strippingFrontmatter("--- not") == "--- not")
    }

    @Test func oversizedAssetsAreLeftOutWithANote() async throws {
        struct Big: ShareVisualRendering {
            func render(_ v: ShareVisual) async throws -> RenderedVisual {
                RenderedVisual(data: Data(count: ShareLimits.assetBytes + 1), contentType: "image/png")
            }
        }
        let result = await ShareSnapshotBuilder(renderer: Big()).build(title: "T", blocks: blocks(["```mermaid\nx\n```"]), theme: .light)
        #expect(result.snapshot.assets.isEmpty)
        #expect(result.snapshot.markdown == "*(Mermaid diagram left out: too large to share)*")
        #expect(result.problems == ["Mermaid diagram left out: too large to share"])
    }

    // MARK: sub-pages

    private func page(_ id: String, parent: String? = "root", _ title: String, _ contents: [String]) -> SharePageSource {
        SharePageSource(docID: id, parentDocID: parent, title: title, blocks: blocks(contents))
    }

    @Test func subPagesGoThroughTheSamePipelineWithTheirOwnAssetNames() async throws {
        let r = FakeVisuals { _ in .svg }
        let root = blocks(["# Plan", "Root", "```mermaid\nroot\n```", "![r](https://ok.example/r.jpg)"])
        let pages = [
            page("c1", "Fleet Twin", ["---\ntags: [a]\n---\n\n# Fleet Twin\n\nFirst", "```mermaid\none\n```", "```reladraw\ntwo\n```", "![a](https://ok.example/a.jpg)"]),
            page("g1", parent: "c1", "Deep", ["# Plan", "```mermaid\nthree\n```"]),
            page("c2", "Empty", []),
        ]
        let result = await ShareSnapshotBuilder(renderer: r, images: FakeImages()).build(title: "Plan", blocks: root, theme: .dark, pages: pages)
        let s = result.snapshot
        #expect(result.problems.isEmpty)
        #expect(s.assets.map(\.name) == ["d1.svg", "i1.jpg", "p1-d1.svg", "p1-d2.svg", "p1-i1.jpg", "p2-d1.svg"])
        #expect(Set(s.assets.map(\.name)).count == s.assets.count && s.assets.allSatisfy { ShareAsset.isValidName($0.name) })
        #expect(s.markdown == "Root\n\n![Mermaid diagram](taisce-asset:d1.svg)\n\n![r](taisce-asset:i1.jpg)", "the root is what it was")
        #expect(s.pages.map(\.docID) == ["c1", "g1", "c2"] && s.pages.map(\.parentDocID) == ["root", "c1", "root"])
        #expect(s.pages.map(\.title) == ["Fleet Twin", "Deep", "Empty"])
        #expect(s.pages[0].markdown == "First\n\n![Mermaid diagram](taisce-asset:p1-d1.svg)\n\n![Reladraw diagram](taisce-asset:p1-d2.svg)\n\n![a](taisce-asset:p1-i1.jpg)", "its own frontmatter and title heading go")
        #expect(s.pages[1].markdown == "# Plan\n\n![Mermaid diagram](taisce-asset:p2-d1.svg)", "only a heading that repeats the page's own title goes")
        #expect(s.pages[2].markdown == "")
        #expect(String(decoding: s.assets[5].data, as: UTF8.self) == "<svg>three</svg>")
        #expect(r.seen.withLock { $0.count == 4 && $0.allSatisfy { $0.theme == .dark } })
        // without pages: byte for byte the single-doc build
        let alone = await ShareSnapshotBuilder(renderer: r, images: FakeImages()).build(title: "Plan", blocks: root, theme: .dark)
        #expect(alone.snapshot.pages.isEmpty && alone.snapshot.markdown == s.markdown && alone.snapshot.assets == Array(s.assets.prefix(2)))
    }

    @Test func aSubPagesProblemsNameThePage() async throws {
        let r = FakeVisuals { $0.source.contains("bad") ? .fail("line 2: bad arrow") : .svg }
        let root = blocks(["```mermaid\nbad root\n```"])
        var canvas = blocks(["```mermaid\nbad\n```", "![x](https://down.example/b.png)", "scene"], types: [.code, .paragraph, .canvasScene])
        canvas[0].docID = "c1"
        let pages = [SharePageSource(docID: "c1", parentDocID: "root", title: "Fleet Twin", blocks: canvas), page("c2", " ", ["```mermaid\nbad\n```"])]
        let result = await ShareSnapshotBuilder(renderer: r, images: FakeImages()).build(title: "Plan", blocks: root, theme: .light, pages: pages)
        #expect(result.problems == [
            "Mermaid diagram couldn't render: line 2: bad arrow",
            "Fleet Twin: Canvases aren't shared",
            "Fleet Twin: Mermaid diagram couldn't render: line 2: bad arrow",
            "Fleet Twin: Image from down.example kept as a link: the server answered 503",
            "Untitled: Mermaid diagram couldn't render: line 2: bad arrow",
        ])
        #expect(result.snapshot.assets.map(\.name) == ["d1-error.svg", "p1-d1-error.svg", "p2-d1-error.svg"])
        #expect(result.snapshot.pages[0].markdown.contains("[x](https://down.example/b.png)"))
    }

    @Test func theImageBudgetIsSharedAcrossPages() async throws {
        // 1.5 MB each (2 MB of base64): five fit in 10 MB, the sixth doesn't
        struct Heavy: ShareVisualRendering {
            func render(_ v: ShareVisual) async throws -> RenderedVisual {
                RenderedVisual(data: Data(count: 1_500_000), contentType: "image/png")
            }
        }
        let fence = "```mermaid\nx\n```"
        let pages = (1...3).map { page("c\($0)", "Page \($0)", [fence, fence]) }
        let result = await ShareSnapshotBuilder(renderer: Heavy()).build(title: "T", blocks: blocks([fence]), theme: .light, pages: pages)
        #expect(result.snapshot.assets.map(\.name) == ["d1.png", "p1-d1.png", "p1-d2.png", "p2-d1.png", "p2-d2.png"])
        #expect(result.problems == ["Page 3: Mermaid diagram left out: too large to share", "Page 3: Mermaid diagram left out: too large to share"])
        #expect(result.snapshot.pages[2].markdown == "*(Mermaid diagram left out: too large to share)*\n\n*(Mermaid diagram left out: too large to share)*")
        #expect(result.snapshot.approximateSize <= ShareLimits.snapshotBytes)
        // a later page's markdown counts before the root's image is let in
        let wordy = [page("c1", "Wordy", [String(repeating: "a", count: ShareLimits.snapshotBytes - 1_000_000)])]
        let squeezed = await ShareSnapshotBuilder(renderer: Heavy()).build(title: "T", blocks: blocks([fence]), theme: .light, pages: wordy)
        #expect(squeezed.snapshot.assets.isEmpty && squeezed.problems == ["Mermaid diagram left out: too large to share"])
    }

    @Test func theAssetCountIsSharedAcrossPages() async throws {
        let r = FakeVisuals { _ in .svg }
        let fences = Array(repeating: "```mermaid\nx\n```", count: 400)
        let pages = [page("c1", "One", fences), page("c2", "Two", fences)]
        let result = await ShareSnapshotBuilder(renderer: r).build(title: "T", blocks: blocks(fences), theme: .light, pages: pages)
        #expect(result.snapshot.assets.count == ShareLimits.assets)
        #expect(result.snapshot.assets.last?.name == "p2-d200.svg")
        #expect(result.problems.count == 200 && result.problems.allSatisfy { $0 == "Two: Mermaid diagram left out: too large to share" })
    }

    @Test func linkedImagesCoverEveryPage() {
        let root = blocks(["![r](https://a.example/r.png)"])
        let pages = [page("c1", "One", ["![x](https://b.example/x.png) ![y](https://a.example/y.png)"]), page("c2", "Two", ["none", "![z](http://c.example/z.png)"])]
        #expect(ShareSnapshotBuilder.linkedImages(title: "T", blocks: root).map(\.absoluteString) == ["https://a.example/r.png"])
        #expect(ShareSnapshotBuilder.linkedImages(title: "T", blocks: root, pages: pages).map(\.absoluteString) == [
            "https://a.example/r.png", "https://b.example/x.png", "https://a.example/y.png", "http://c.example/z.png",
        ])
    }
}
