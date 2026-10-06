import Foundation
import PDFKit
import Synchronization
import Testing
import TaisceKit
import UIKit
@testable import Taisce

/// A scripted `/api/shares`: remembers what was asked.
private final class FakeShares: ShareService, @unchecked Sendable {
    let lock = Mutex<[String]>([])
    var calls: [String] { lock.withLock { $0 } }
    private func log(_ s: String) { lock.withLock { $0.append(s) } }

    static func share(_ id: String, doc: String = "d1", expires: Date? = nil, revoked: Date? = nil, comments: Int = 0, unread: Int = 0, created: Double = 0) -> Share {
        Share(id: id, docID: doc, url: "https://taisce.example/s/\(id)", createdAt: Date(timeIntervalSince1970: created), expiresAt: expires, revokedAt: revoked, commentCount: comments, unreadComments: unread, title: "Plan")
    }

    let existing: [Share]
    let comments: [ShareComment]
    init(existing: [Share] = [], comments: [ShareComment] = []) {
        self.existing = existing
        self.comments = comments
    }

    func createShare(docID: DocID, snapshot: ShareSnapshot, expiresAt: Date?, commentsEnabled: Bool) async throws -> Share {
        log("create \(docID) \(snapshot.markdown) \(expiresAt == nil ? "never" : "dated") comments=\(commentsEnabled) theme=\(snapshot.theme.rawValue)")
        var s = Self.share("new", doc: docID, expires: expiresAt, created: 100)
        s.commentsEnabled = commentsEnabled
        return s
    }

    func shares(docID: DocID?) async throws -> [Share] {
        log("list \(docID ?? "all")")
        return existing.filter { docID == nil || $0.docID == docID }
    }

    func updateShare(_ id: String, snapshot: ShareSnapshot?, expiresAt: ShareExpiryChange, commentsEnabled: Bool?) async throws -> Share {
        log("patch \(id) snapshot=\(snapshot != nil) expiry=\(expiresAt == .keep ? "keep" : "set") comments=\(commentsEnabled.map(String.init) ?? "-")")
        var s = existing.first { $0.id == id } ?? Self.share(id)
        if snapshot != nil { s.revision += 1 }
        if case let .set(d) = expiresAt { s.expiresAt = d }
        if let commentsEnabled { s.commentsEnabled = commentsEnabled }
        return s
    }

    func revokeShare(_ id: String) async throws { log("revoke \(id)") }
    func sharePreviewHTML(_ snapshot: ShareSnapshot) async throws -> String { "<p>\(snapshot.markdown)</p>" }

    func shareComments(_ shareID: String) async throws -> [ShareComment] {
        log("comments \(shareID)")
        return comments
    }

    func replyToShareComment(_ shareID: String, body: String, parentID: String?, anchor: ShareAnchor?) async throws -> ShareComment {
        log("reply \(shareID) \(parentID ?? "-") \(body)")
        return ShareComment(id: "r", parentID: parentID, author: "Tom", isOwner: true, body: body)
    }

    func deleteShareComment(_ shareID: String, commentID: String) async throws { log("delete \(shareID)/\(commentID)") }
}

@MainActor
private func store(_ fake: FakeShares) -> ShareLinkStore {
    let s = ShareLinkStore()
    s.serviceOverride = fake
    s.snapshotOverride = { doc, theme in
        ShareSnapshotResult(snapshot: ShareSnapshot(title: "Plan", markdown: "md-\(doc)", theme: theme), problems: ["Chart couldn't render: x"])
    }
    return s
}

@MainActor @Suite struct ShareLinkStoreTests {
    @Test func createPublishesTheSnapshotAndKeepsTheLink() async throws {
        let fake = FakeShares()
        let s = store(fake)
        let share = try await s.create(doc: "d1", expiresAt: nil, commentsEnabled: false, theme: .dark)
        #expect(fake.calls == ["create d1 md-d1 never comments=false theme=dark"])
        #expect(s.shares(for: "d1") == [share])
        #expect(s.lastProblems == ["Chart couldn't render: x"])
    }

    @Test func liveLinksSortFirst() async throws {
        let past = Date.now.addingTimeInterval(-60)
        let fake = FakeShares(existing: [
            FakeShares.share("old", expires: past, created: 5),
            FakeShares.share("live1", created: 1),
            FakeShares.share("live2", created: 3),
            FakeShares.share("gone", revoked: past, created: 9),
        ])
        let s = store(fake)
        try await s.load(doc: "d1")
        #expect(s.shares(for: "d1").map(\.id) == ["live2", "live1", "gone", "old"])
    }

    @Test func updateExpiryCommentsAndRevoke() async throws {
        let fake = FakeShares(existing: [FakeShares.share("s1")])
        let s = store(fake)
        try await s.loadAll()
        let share = try #require(s.all.first)
        try await s.republish(share, theme: .light)
        try await s.setExpiry(share, to: nil)
        try await s.setComments(share, enabled: false)
        try await s.revoke(share)
        #expect(fake.calls == [
            "list all",
            "patch s1 snapshot=true expiry=keep comments=-",
            "patch s1 snapshot=false expiry=set comments=-",
            "patch s1 snapshot=false expiry=keep comments=false",
            "revoke s1",
        ])
        let after = try #require(s.all.first)
        #expect(after.state() == .revoked)
        #expect(s.shares(for: "d1").first?.state() == .revoked)
    }

    @Test func readingCommentsClearsUnreadAndReplyGoesToTheThread() async throws {
        let root = ShareComment(id: "c1", author: "Aoife", body: "Nice", anchor: ShareAnchor(block: 2, quote: "q"))
        let fake = FakeShares(existing: [FakeShares.share("s1", comments: 1, unread: 1)], comments: [root])
        let s = store(fake)
        try await s.loadAll()
        #expect(s.unreadTotal == 1)
        #expect(s.commentSummary(for: "d1").title == "Comments from shared links (1 new)")
        let share = try #require(s.all.first)
        let list = try await s.comments(for: share)
        #expect(list == [root])
        #expect(s.unreadTotal == 0)
        #expect(s.commentSummary(for: "d1").title == "Comments from shared links")
        _ = try await s.reply(to: s.all[0], body: "  Thanks  ", parent: root)
        try await s.delete(root, on: s.all[0])
        #expect(fake.calls.suffix(2) == ["reply s1 c1 Thanks", "delete s1/c1"])
    }

    @Test func resetForgetsEverything() async throws {
        let s = store(FakeShares(existing: [FakeShares.share("s1")]))
        try await s.loadAll()
        s.reset()
        #expect(s.all.isEmpty && s.shares(for: "d1").isEmpty && !s.allLoaded)
    }

    @Test func hiddenWithoutASignedInServer() {
        let m = AppModel()
        // LOCAL / signed out / still checking: no share links
        #expect(!m.shareLinks.isAvailable)
    }
}

@MainActor @Suite struct ShareLinkTextTests {
    @Test func summaryHidesWhenThereAreNoComments() {
        #expect(LinkCommentSummary(total: 0, unread: 0).title == nil)
        #expect(LinkCommentSummary(total: 3, unread: 2).title == "Comments from shared links (2 new)")
    }

    @Test func stateAndViews() {
        let now = Date(timeIntervalSince1970: 1_000_000)
        var s = FakeShares.share("s")
        #expect(ShareLinkText.state(s, now: now) == "Never expires")
        #expect(ShareLinkText.views(s, now: now) == "Not opened yet")
        s.views = 3
        s.lastViewedAt = now.addingTimeInterval(-120)
        #expect(ShareLinkText.views(s, now: now) == "3 views · last opened 2 min ago")
        s.revokedAt = now
        #expect(ShareLinkText.state(s, now: now) == "Revoked")
        s.revokedAt = nil
        s.expiresAt = now.addingTimeInterval(-1)
        #expect(ShareLinkText.state(s, now: now) == "Expired")
        #expect(ShareLinkText.comments(FakeShares.share("c", comments: 2, unread: 1)) == "2 comments · 1 new")
        var off = FakeShares.share("o")
        off.commentsEnabled = false
        #expect(ShareLinkText.comments(off) == "Comments off")
    }

    @Test func pdfFileNamesAndPaper() {
        #expect(PDFExportFile.filename("Q3 plan: v2/final") == "Q3 plan- v2-final.pdf")
        #expect(PDFExportFile.filename("  ..  ") == "Untitled.pdf")
        #expect(PaperSize.for(region: "US") == .letter)
        #expect(PaperSize.for(region: "IE") == .a4)
        #expect(PaperSize.for(region: nil) == .a4)
    }
}

@MainActor @Suite struct ShareCommandTests {
    private let picker = WorkspacePicker(workspaces: [], unsortedCount: 0, stored: nil)

    @Test func shareAndExportNeedServerModeAndADoc() {
        let r = Router()
        #expect(!r.canPerform(.shareLink, signedIn: true, picker: picker, shares: true))
        r.open(.doc("d1"))
        #expect(r.canPerform(.shareLink, signedIn: true, picker: picker, shares: true))
        #expect(r.canPerform(.exportPDF, signedIn: true, picker: picker, shares: true))
        #expect(!r.canPerform(.exportPDF, signedIn: true, picker: picker, shares: false))
        r.editingDoc = "d1"
        #expect(!r.canPerform(.shareLink, signedIn: true, picker: picker, shares: true))
        r.editingDoc = nil
        _ = r.perform(.exportPDF, picker: picker)
        #expect(r.docAction?.doc == "d1" && r.docAction?.kind == .exportPDF)
        _ = r.perform(.shareLink, picker: picker)
        #expect(r.docAction?.kind == .shareLink && r.docAction?.serial == 2)
    }

    @Test func aCommentPushOpensTheDocsLinkComments() {
        let r = Router()
        r.openLinkComments("d9")
        #expect(r.focusedDoc == "d9" && r.docAction?.kind == .linkComments)
        let pad = Router()
        pad.isPad = true
        pad.openLinkComments("d9")
        #expect(pad.padItem == .doc("d9") && pad.docAction?.doc == "d9")
    }

    @Test func pushPayload() {
        #expect(ShareCommentPush.docID(from: ["kind": "share_comment", "doc_id": "d1", "share_id": "s1"]) == "d1")
        #expect(ShareCommentPush.docID(from: ["aps": ["content-available": 1], "seq": 3]) == nil)
        #expect(ShareCommentPush.docID(from: ["kind": "share_comment"]) == nil)
    }
}

/// Real renders: the bundled mermaid / reladraw to SVG, Swift Charts to PNG,
/// and a long page printed to more than one PDF page.
@MainActor
@Suite(.serialized) struct ShareRenderSmokeTests {
    @Test func diagramsRenderToSVG() async throws {
        let r = AppShareVisualRenderer()
        let m = try await r.render(ShareVisual(kind: .mermaid, source: "flowchart LR\n  A[Phone] --> B(Server)", theme: .dark))
        #expect(m.contentType == "image/svg+xml")
        let svg = String(decoding: m.data, as: UTF8.self)
        #expect(svg.hasPrefix("<?xml") && svg.contains("<svg") && svg.contains("xmlns=\"http://www.w3.org/2000/svg\""))
        #expect((m.width ?? 0) > 50 && (m.width ?? 0) <= AppShareVisualRenderer.width)
        let rd = try await r.render(ShareVisual(kind: .reladraw, source: "node a \"Phone\"\nnode b \"Server\" right of a\nedge a -> b", theme: .light))
        #expect(String(decoding: rd.data, as: UTF8.self).contains("<svg"))
        await #expect(throws: DiagramRenderError.self) {
            _ = try await r.render(ShareVisual(kind: .reladraw, source: "node a\nedge a -> zz", theme: .light))
        }
    }

    @Test func chartsRenderToPNG() async throws {
        let spec = #"{"mark":"bar","title":"Sales","data":{"values":[{"m":"Jan","v":3},{"m":"Feb","v":5}]},"encoding":{"x":{"field":"m"},"y":{"field":"v","type":"quantitative"}}}"#
        let out = try await AppShareVisualRenderer().render(ShareVisual(kind: .vegaLite, source: spec, theme: .light))
        #expect(out.contentType == "image/png" && out.alt == "Sales")
        #expect(UIImage(data: out.data) != nil)
        await #expect(throws: DiagramRenderError.self) {
            _ = try await AppShareVisualRenderer().render(ShareVisual(kind: .vegaLite, source: "{not json", theme: .light))
        }
    }

    @Test func aWholeDocBuildsWithDiagramsAndAFailure() async throws {
        let blocks = ["Intro", "```mermaid\nflowchart LR\n  A --> B\n```", "```reladraw\nnode a\nedge a -> zz\n```"].enumerated().map { i, c in
            Block(id: "b\(i)", docID: "d", parentID: nil, orderKey: "\(i)", blockType: .paragraph, content: c)
        }
        let result = await ShareSnapshotBuilder(renderer: AppShareVisualRenderer(), timeout: .seconds(30)).build(title: "T", blocks: blocks, theme: .light)
        #expect(result.snapshot.assets.map(\.name) == ["d1.svg", "d2-error.svg"])
        #expect(result.problems == ["Reladraw diagram couldn't render: line 2: edge to \"zz\", which does not exist"])
    }

    @Test func longPagesPrintToSeveralPDFPages() async throws {
        let paras = (1...120).map { "<p>Paragraph \($0): the quick brown fox jumps over the lazy dog, again and again.</p>" }.joined()
        let html = "<!doctype html><html><head><meta charset=utf-8><style>body{font:14px Georgia}</style></head><body><h1>Long</h1>\(paras)<img src=\"data:image/svg+xml;base64,\(Data("<svg xmlns='http://www.w3.org/2000/svg' width='10' height='10'/>".utf8).base64EncodedString())\"></body></html>"
        let (data, layout) = try await PDFRenderer.pdf(html: html, title: "Long", paper: .a4)
        let doc = try #require(PDFDocument(data: data))
        guard case let .paged(pages) = layout else {
            Issue.record("expected pages, got \(layout)")
            return
        }
        #expect(pages > 1 && doc.pageCount == pages)
        let box = try #require(doc.page(at: 0)?.bounds(for: .mediaBox))
        #expect(abs(box.width - 595.2) < 1 && abs(box.height - 841.8) < 1)
        #expect(doc.string?.contains("Paragraph 120") == true)
    }
}
