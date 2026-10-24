import Foundation
import Testing
@testable import TaisceKit

/// Today's review queue, book in progress, Ask, e-readers and upload
/// tokens: the wire shapes as crates/daemon sends them.
@Suite struct HomeAPITests {
    /// `/api/queue` as `decorate_review_items` writes it (api.rs).
    static let queue = """
    [{"current_content":null,"doc_title":"Reading","proposer":"filer",
      "item":{"annotation":{"doc_id":"d1","id":"a1","kind":"review","op_id":"o1","resolved_by":null,"status":"open"},
              "op":{"base_epoch":0,"confidence":1.0,"doc_id":"d1","epoch_applied":0,"id":"o1",
                    "kind":{"from_title":"Reading notes","op":"rename_doc","title":"Reading"},
                    "principal":"p1","prior":null,"source_refs":["mcp:rename_doc"],"verdict":"yellow"}}},
     {"current_content":"Every change is a proposal, from a person or an agent.","doc_title":"Architecture","proposer":"keeper:API",
      "item":{"annotation":{"doc_id":"d2","id":"a2","kind":"parked","op_id":"o2","resolved_by":null,"status":"open"},
              "op":{"base_epoch":1,"doc_id":"d2","id":"o2","kind":{"op":"replace","target":"b1","content":"An edit lands yellow."},
                    "principal":"p2","prior":{"content":"Every change is a proposal."},"verdict":"red"}}},
     {"doc_title":"Old","proposer":"claude:x",
      "item":{"annotation":{"doc_id":"d3","id":"a3","kind":"review","op_id":"o3","status":"accepted"},"op":{"id":"o3","doc_id":"d3"}}}]
    """

    @Test func reviewQueueDecodesOpenItems() async throws {
        let server = MockServer { r in
            #expect(r.path == "/api/queue")
            return .json(Self.queue)
        }
        let items = try await server.client().reviewQueue()
        #expect(items.map(\.id) == ["a1", "a2"], "a resolved row never shows")
        #expect(items[0].kind == .flagged && items[0].summary == "Rename to \u{201C}Reading\u{201D}")
        #expect(items[0].byAgent == false && items[1].byAgent == true)
        #expect(items[1].kind == .parked && items[1].proposed == "An edit lands yellow.")
        #expect(items[1].prior == "Every change is a proposal." && items[1].docID == "d2")
        let s = ReviewQueueSummary(items)
        #expect(s.flagged == 1 && s.parked == 1 && s.total == 2)
        // the Library's per-doc flag counts come from the same read
        #expect(ReviewQueueItem.counts(items) == ["d1": 1, "d2": 1])
    }

    @Test func resolveSendsTheDecision() async throws {
        let server = MockServer { _ in .json(#"{"ok":true,"receipt":{}}"#) }
        try await server.client().resolveQueued("a1", .decline)
        let r = server.requests[0]
        #expect(r.path == "/api/resolve" && r.httpMethod == "POST")
        let body = try JSONSerialization.jsonObject(with: try #require(r.httpBody)) as? [String: String]
        #expect(body == ["annotation_id": "a1", "decision": "decline"])
    }

    @Test func resolveSurfacesTheServersRefusal() async throws {
        let server = MockServer { _ in .json(#"{"error":"proposer cannot approve own proposal"}"#) }
        await #expect(throws: APIError.server("proposer cannot approve own proposal")) {
            try await server.client().resolveQueued("a1", .accept)
        }
    }

    @Test func askDecodesTheAnswerAndNothing() async throws {
        let server = MockServer { r in
            let body = (try? JSONSerialization.jsonObject(with: r.httpBody ?? Data())) as? [String: String]
            return body?["question"] == "zebra?"
                ? .json(#"{"doc_id":null,"title":"zebra","sources":0,"docs":0}"#)
                : .json(#"{"doc_id":"ans1","title":"How","sources":4,"docs":2}"#)
        }
        let a = try await server.client().ask("how does the gate work?")
        #expect(a == AskAnswer(docID: "ans1", title: "How", sources: 4, docs: 2))
        let none = try await server.client().ask("zebra?")
        #expect(none.docID == nil)
    }

    @Test func ereadersAndTokensRoundTrip() async throws {
        let server = MockServer { r in
            switch (r.httpMethod ?? "GET", r.path) {
            case ("GET", "/api/ereaders"):
                return .json(#"{"ereaders":[{"id":"e1","label":"Kindle","stream_downloads":true,"created_at":"2026-10-01T00:00:00Z","last_used_at":"2026-10-24T08:00:00Z","revoked_at":null}]}"#)
            case ("POST", "/api/ereaders"):
                return Reply(status: 201, chunks: [Data(#"{"ereader":{"id":"e2","label":"Kobo","stream_downloads":false,"created_at":"2026-10-24T09:00:00Z"},"password":"abcd-efgh-ijkl-mnop"}"#.utf8)])
            case ("POST", "/api/ereaders/e1/rotate"):
                return .json(#"{"password":"qrst-uvwx-yz23-4567"}"#)
            case ("DELETE", "/api/ereaders/e1"), ("DELETE", "/api/upload-tokens/t1"):
                return Reply(status: 204, chunks: [])
            case ("GET", "/api/upload-tokens"):
                return .json(#"{"upload_tokens":[{"id":"t1","label":"Shortcut","created_at":"2026-10-01T00:00:00Z","last_used_at":null,"revoked_at":null},{"id":"t0","label":"Old","created_at":"2026-01-01T00:00:00Z","revoked_at":"2026-02-01T00:00:00Z"}]}"#)
            case ("POST", "/api/upload-tokens"):
                return Reply(status: 201, chunks: [Data(#"{"upload_token":{"id":"t2","label":"Calibre","created_at":"2026-10-24T09:00:00Z"},"token":"tup_secret"}"#.utf8)])
            default:
                return Reply(status: 404, chunks: [])
            }
        }
        let api = server.client()
        let readers = try await api.ereaders()
        #expect(readers.map(\.label) == ["Kindle"] && readers[0].streamDownloads && readers[0].isActive)
        let (e, password) = try await api.pairEreader(label: "Kobo", streamDownloads: false)
        #expect(e.id == "e2" && password == "abcd-efgh-ijkl-mnop")
        let pair = try JSONSerialization.jsonObject(with: try #require(server.requests[1].httpBody)) as? [String: Any]
        #expect(pair?["label"] as? String == "Kobo" && pair?["stream_downloads"] as? Bool == false)
        #expect(try await api.rotateEreader("e1") == "qrst-uvwx-yz23-4567")
        try await api.removeEreader("e1")
        let tokens = try await api.uploadTokens()
        #expect(tokens.filter(\.isActive).map(\.id) == ["t1"])
        let (t, secret) = try await api.createUploadToken(label: "Calibre")
        #expect(t.id == "t2" && secret == "tup_secret")
        try await api.revokeUploadToken("t1")
    }

    @Test func deviceSetupMatchesTheWeb() {
        let s = DevicePasswordSetup(server: "https://taisce.null.ie/", username: "Tom", password: "k7mq-4xpt")
        #expect(s.opds == "https://taisce.null.ie/opds/")
        #expect(s.opdsPath == "https://taisce.null.ie/opds/k7mq-4xpt/")
        #expect(s.progressSync == "https://taisce.null.ie/kosync")
        #expect(s.username == "Tom")
    }

    typealias Reply = MockServer.Reply
}
