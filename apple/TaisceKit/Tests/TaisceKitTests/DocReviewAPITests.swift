import Foundation
import Testing
@testable import TaisceKit

@Suite struct DocReviewAPITests {
    /// the daemon's /api/doc/{id}/review rows, as decorated by decorate_review_items
    static let rows = """
    [{"current_content":"Every change is a proposal. Agents' edits are always yellow.","doc_title":"Architecture",
      "item":{"annotation":{"doc_id":"d1","id":"a1","kind":"parked","op_id":"0199a0b0-c0d0-7abc-8def-0123456789ab","resolved_by":null,"status":"open"},
              "op":{"base_epoch":1,"confidence":0.2,"doc_id":"d1","epoch_applied":null,"id":"0199a0b0-c0d0-7abc-8def-0123456789ab",
                    "kind":{"content":"Every change is a proposal. An edit lands yellow.","op":"replace","target":"b1"},
                    "prior":{"block_type":"paragraph","content":"Every change is a proposal. Agents' edits are always yellow.","id":"b1"},
                    "source_refs":[],"verdict":"red"}},
      "proposer":"keeper:API"},
     {"current_content":null,"doc_title":"Architecture",
      "item":{"annotation":{"doc_id":"d1","id":"a2","kind":"review","op_id":"o2","resolved_by":null,"status":"open"},
              "op":{"confidence":0.8,"kind":{"block_id":"b2","block_type":"paragraph","content":"Litestream streams the WAL.","op":"insert","order_key":"a","parent_id":"h1"},"prior":null}},
      "proposer":"tom"},
     {"doc_title":"Architecture","item":{"annotation":{"doc_id":"d1","id":"a3","kind":"review","op_id":"o3","status":"accepted"},"op":{"kind":{"op":"delete","target":"b3"}}},"proposer":"x"}]
    """

    @Test func docReviewDecodesFlaggedAndParked() async throws {
        let server = MockServer { r in
            #expect(r.path == "/api/doc/d1/review")
            return .json(Self.rows)
        }
        let items = try await server.client().docReview("d1")
        #expect(items.map(\.id) == ["a1", "a2"], "resolved items are left out")
        let parked = items[0]
        #expect(parked.kind == .parked)
        #expect(parked.opType == "replace")
        #expect(parked.block == "b1")
        #expect(parked.proposer == "keeper:API")
        #expect(parked.byAgent)
        #expect(parked.before == "Every change is a proposal. Agents' edits are always yellow.")
        #expect(parked.after == "Every change is a proposal. An edit lands yellow.")
        #expect(parked.date == Date(timeIntervalSince1970: Double(0x0199a0b0c0d0) / 1000))
        #expect(parked.summary == "Rewrite a paragraph")
        let flagged = items[1]
        #expect(flagged.kind == .flagged)
        #expect(flagged.block == "b2", "an insert names its new block")
        #expect(flagged.before == "")
        #expect(flagged.after == "Litestream streams the WAL.")
        #expect(!flagged.byAgent)
        #expect(flagged.summary == "Added a paragraph")
        #expect(DocReviewItem.counts(items) == ["d1": 2])
    }

    @Test func summariesNameHeadings() {
        let del = DocReviewItem(id: "a", docID: "d", opID: "o", kind: .parked, opType: "delete", block: "b", prior: "## Federation (cut)", proposer: "claude:docs")
        #expect(del.summary == "Delete \u{201C}Federation (cut)\u{201D}")
        #expect(del.after == "")
        #expect(del.before == "## Federation (cut)")
    }

    @Test func resolvePostsTheDecision() async throws {
        let server = MockServer { r in
            #expect(r.path == "/api/resolve")
            return .json(#"{"ok":true,"receipt":{}}"#)
        }
        try await server.client().resolveReview("a1", .decline)
        let body = try #require(server.requests.first?.httpBody)
        let json = try #require(try JSONSerialization.jsonObject(with: body) as? [String: String])
        #expect(json == ["annotation_id": "a1", "decision": "decline"])
    }

    @Test func backlinksSkipDeletedAndSelf() async throws {
        let server = MockServer { _ in
            .json("""
            [{"block":{"id":"x1","doc_id":"d2","content":"Read [[Architecture]] first.","deleted":false},"doc_title":"Agent Guide"},
             {"block":{"id":"x2","doc_id":"d3","content":"gone","deleted":true},"doc_title":"Old"},
             {"block":{"id":"x3","doc_id":"d1","content":"self","deleted":false},"doc_title":"Architecture"}]
            """)
        }
        let links = try await server.client().backlinks("d1")
        #expect(links == [Backlink(docID: "d2", docTitle: "Agent Guide", blockID: "x1", content: "Read [[Architecture]] first.")])
    }
}
