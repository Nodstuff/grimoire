import Foundation

/// One open item of a doc's review queue (`GET /api/doc/{id}/review`, the
/// same shape as `/api/queue`): an op the gate flagged (applied, yellow:
/// Keep or Revert) or parked (not applied, red: Apply or Discard).
public struct DocReviewItem: Decodable, Sendable, Hashable, Identifiable {
    public enum Kind: String, Sendable, Hashable {
        /// applied, flagged for a human (yellow)
        case flagged
        /// not applied until a human accepts it (red)
        case parked
    }

    /// the annotation id: what a decision names
    public var id: String
    public var docID: DocID
    public var opID: String
    public var kind: Kind
    public var open: Bool
    /// `insert`, `replace`, `delete`, `move`, …
    public var opType: String
    /// the block the op writes (an insert's new block)
    public var block: BlockID?
    /// what an insert or replace writes
    public var content: String?
    /// the block's text before the op (nil for an insert)
    public var prior: String?
    /// the block's live text now (what a parked replace would change)
    public var current: String?
    /// who proposed it ("keeper:API", "tom")
    public var proposer: String
    public var docTitle: String
    /// the gate's confidence, 0…1
    public var confidence: Double?

    public init(id: String, docID: DocID, opID: String, kind: Kind, open: Bool = true, opType: String, block: BlockID? = nil,
                content: String? = nil, prior: String? = nil, current: String? = nil, proposer: String, docTitle: String = "",
                confidence: Double? = nil) {
        self.id = id
        self.docID = docID
        self.opID = opID
        self.kind = kind
        self.open = open
        self.opType = opType
        self.block = block
        self.content = content
        self.prior = prior
        self.current = current
        self.proposer = proposer
        self.docTitle = docTitle
        self.confidence = confidence
    }

    /// When the op was written (its UUIDv7 id).
    public var date: Date? { DocHistoryEntry.uuidV7Date(opID) }

    /// An agent proposed it (agent principals are named `who:what`).
    public var byAgent: Bool { proposer.contains(":") }

    /// The text before and after, for a word diff: a flagged replace shows
    /// prior → content (applied); a parked one current → content; a delete
    /// removes everything; an insert adds everything.
    public var before: String {
        switch opType {
        case "insert": ""
        default: (kind == .parked ? (current ?? prior) : prior) ?? ""
        }
    }

    public var after: String {
        switch opType {
        case "delete": ""
        default: content ?? ""
        }
    }

    /// What the op does, in words: "Rewrote a paragraph", "Delete “Federation (cut)”".
    public var summary: String {
        let subject = Self.subject(prior ?? current ?? content ?? "")
        switch opType {
        case "insert": return "Added \(subject)"
        case "delete": return kind == .parked ? "Delete \(subject)" : "Deleted \(subject)"
        case "move": return "Moved \(subject)"
        case "replace": return kind == .parked ? "Rewrite \(subject)" : "Rewrote \(subject)"
        default: return opType.replacingOccurrences(of: "_", with: " ").capitalized
        }
    }

    /// A heading by its words in quotes; anything else by what it is.
    static func subject(_ markdown: String) -> String {
        let t = markdown.trimmingCharacters(in: .whitespacesAndNewlines)
        if t.hasPrefix("#") {
            let words = t.drop(while: { $0 == "#" }).trimmingCharacters(in: .whitespaces)
            return "\u{201C}\(words)\u{201D}"
        }
        if t.hasPrefix("```") { return "a code block" }
        if t.hasPrefix("- [") || t.hasPrefix("* [") { return "a to-do list" }
        if t.hasPrefix("- ") || t.hasPrefix("* ") || t.first?.isNumber == true && t.contains(". ") { return "a list" }
        if t.hasPrefix(">") { return "a quote" }
        if t.hasPrefix("|") { return "a table" }
        return t.isEmpty ? "a block" : "a paragraph"
    }

    struct Wire: Decodable {
        struct Item: Decodable {
            struct Annotation: Decodable {
                var id: String
                var doc_id: String
                var op_id: String
                var kind: String
                var status: String?
            }
            struct Op: Decodable {
                struct Kind: Decodable {
                    var op: String?
                    var target: String?
                    var block_id: String?
                    var content: String?
                }
                struct Prior: Decodable { var content: String? }
                var kind: Kind?
                var prior: Prior?
                var confidence: Double?
            }
            var annotation: Annotation
            var op: Op?
        }
        var item: Item
        var doc_title: String?
        var proposer: String?
        var current_content: String?
    }

    public init(from decoder: any Decoder) throws {
        let w = try Wire(from: decoder)
        let a = w.item.annotation
        id = a.id
        docID = a.doc_id
        opID = a.op_id
        kind = a.kind == "parked" ? .parked : .flagged
        open = (a.status ?? "open") == "open"
        let k = w.item.op?.kind
        opType = k?.op ?? "replace"
        block = k?.target ?? k?.block_id
        content = k?.content
        prior = w.item.op?.prior?.content
        current = w.current_content
        proposer = w.proposer ?? ""
        docTitle = w.doc_title ?? ""
        confidence = w.item.op?.confidence
    }
}

/// A doc that links here (`GET /api/doc/{id}/backlinks`): the linking block.
public struct Backlink: Decodable, Sendable, Hashable, Identifiable {
    public var docID: DocID
    public var docTitle: String
    public var blockID: BlockID
    /// the linking block's markdown
    public var content: String
    public var id: BlockID { blockID }

    public init(docID: DocID, docTitle: String, blockID: BlockID, content: String) {
        self.docID = docID
        self.docTitle = docTitle
        self.blockID = blockID
        self.content = content
    }

    struct Wire: Decodable {
        struct B: Decodable { var id: String; var doc_id: String; var content: String; var deleted: Bool? }
        var block: B
        var doc_title: String?
    }

    public init(from decoder: any Decoder) throws {
        let w = try Wire(from: decoder)
        docID = w.block.doc_id
        docTitle = w.doc_title ?? ""
        blockID = w.block.id
        content = w.block.content
    }
}

/// A human's decision on a review item.
public enum DocReviewDecision: String, Sendable, Encodable {
    /// Keep (flagged) / Apply (parked)
    case accept
    /// Revert (flagged) / Discard (parked)
    case decline
}

extension APIClient {
    /// The doc's review queue: open items first as the server orders them.
    public func docReview(_ docID: DocID) async throws -> [DocReviewItem] {
        let rows: [DocReviewItem] = try await get("/api/doc/\(docID)/review")
        return rows.filter(\.open)
    }

    /// Every open review item the caller can see, across docs.
    public func reviewQueue() async throws -> [DocReviewItem] {
        let rows: [DocReviewItem] = try await get("/api/queue")
        return rows.filter(\.open)
    }

    /// Decide an item as the signed-in person (`POST /api/resolve`).
    public func resolveReview(_ annotationID: String, _ decision: DocReviewDecision) async throws {
        struct Req: Encodable { var annotation_id: String; var decision: DocReviewDecision }
        struct Ok: Decodable { var ok: Bool? }
        let _: Ok = try await post("/api/resolve", body: Req(annotation_id: annotationID, decision: decision))
    }

    /// Blocks in other docs that link to this one (deleted ones left out).
    public func backlinks(_ docID: DocID) async throws -> [Backlink] {
        struct Row: Decodable {
            var link: Backlink
            var deleted: Bool
            init(from decoder: any Decoder) throws {
                link = try Backlink(from: decoder)
                deleted = (try? Backlink.Wire(from: decoder).block.deleted) ?? false
            }
        }
        let rows: [Row] = try await get("/api/doc/\(docID)/backlinks")
        return rows.filter { !$0.deleted && $0.link.docID != docID }.map(\.link)
    }
}

extension DocReviewItem {
    /// Open items per doc (the Library's flag counts).
    public static func counts(_ items: [DocReviewItem]) -> [DocID: Int] {
        var out: [DocID: Int] = [:]
        for i in items where i.open { out[i.docID, default: 0] += 1 }
        return out
    }
}
