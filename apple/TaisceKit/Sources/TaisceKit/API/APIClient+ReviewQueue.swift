import Foundation

/// One open item of the global review queue (`GET /api/queue`): a change
/// waiting for a person. `flagged` changes are already applied (keep or
/// revert); `parked` ones never applied (apply or discard). The per-doc
/// queue the doc screen shows is a separate surface (`DocReview*`).
public struct ReviewQueueItem: Sendable, Hashable, Identifiable {
    public enum Kind: String, Sendable, Hashable {
        /// yellow: applied, flagged for a person
        case flagged
        /// red: parked, not applied
        case parked
    }

    /// the annotation id (what `resolveQueued` takes)
    public var id: String
    public var docID: DocID
    public var docTitle: String
    public var kind: Kind
    /// who proposed it (an agent's `claude:docs`, or a person's name)
    public var proposer: String
    /// what the change does, in a few words ("Rename to “Reading”")
    public var summary: String
    /// the proposed text, for an edit (nil for doc ops)
    public var proposed: String?
    /// the block's text now (nil when the change made it, or for doc ops)
    public var current: String?
    /// the text before a flagged edit applied (the op's prior)
    public var prior: String?

    public init(id: String, docID: DocID, docTitle: String, kind: Kind, proposer: String, summary: String, proposed: String? = nil, current: String? = nil, prior: String? = nil) {
        self.id = id
        self.docID = docID
        self.docTitle = docTitle
        self.kind = kind
        self.proposer = proposer
        self.summary = summary
        self.proposed = proposed
        self.current = current
        self.prior = prior
    }

    /// A person resolves; an agent never does. Agents are named with a colon.
    public var byAgent: Bool { proposer.contains(":") }

    /// Open items per doc (the Library's flag counts), from one `/api/queue` read.
    public static func counts(_ items: [ReviewQueueItem]) -> [DocID: Int] {
        var out: [DocID: Int] = [:]
        for i in items { out[i.docID, default: 0] += 1 }
        return out
    }
}

/// One row of `/api/queue` as sent: the item, and whether it is still open.
struct ReviewQueueRow: Decodable {
    var item: ReviewQueueItem
    var open: Bool

    private struct Wire: Decodable {
        struct Item: Decodable {
            struct Annotation: Decodable {
                var id: String
                var doc_id: String
                var kind: String
                var status: String?
            }
            struct Op: Decodable {
                var kind: OpKind?
                var prior: Prior?
            }
            var annotation: Annotation
            var op: Op
        }
        struct Prior: Decodable { var content: String? }
        /// `OpKind` as the store serialises it: `{"op": "replace", …}`.
        struct OpKind: Decodable {
            var op: String
            var content: String?
            var title: String?
            var from_title: String?
            var block_type: String?
        }
        var item: Item
        var doc_title: String?
        var proposer: String?
        var current_content: String?
    }

    init(from decoder: any Decoder) throws {
        let w = try Wire(from: decoder)
        let a = w.item.annotation
        let kind: ReviewQueueItem.Kind = a.kind == "parked" ? .parked : .flagged
        let op = w.item.op.kind
        item = ReviewQueueItem(
            id: a.id, docID: a.doc_id, docTitle: w.doc_title ?? "", kind: kind, proposer: w.proposer ?? "",
            summary: ReviewQueueItem.summary(op: op?.op, title: op?.title, fromTitle: op?.from_title, docTitle: w.doc_title),
            proposed: op?.content, current: w.current_content, prior: w.item.op.prior?.content
        )
        open = (a.status ?? "open") == "open"
    }
}

extension ReviewQueueItem {
    static func summary(op: String?, title: String?, fromTitle: String?, docTitle: String?) -> String {
        switch op {
        case "replace": "Edit a paragraph"
        case "insert": "Add a paragraph"
        case "delete": "Delete a paragraph"
        case "move": "Move a paragraph"
        case "rename_doc":
            if let title { "Rename to \u{201C}\(title)\u{201D}" } else { "Rename the doc" }
        case "delete_doc": "Move \u{201C}\(docTitle ?? "this doc")\u{201D} to the Trash"
        case "move_doc": "Move the doc"
        case "set_status": "Change the doc's status"
        case let other?: other.replacingOccurrences(of: "_", with: " ").capitalizedFirst
        case nil: "Change"
        }
    }
}

private extension String {
    var capitalizedFirst: String { prefix(1).uppercased() + dropFirst() }
}

/// What a person decides on a queued change.
public enum ReviewQueueDecision: String, Sendable {
    /// keep a flagged change / apply a parked one
    case accept
    /// revert a flagged change / discard a parked one
    case decline
}

/// Counts for Today's Review card and the sidebar.
public struct ReviewQueueSummary: Sendable, Hashable {
    public var flagged: Int
    public var parked: Int
    public var total: Int { flagged + parked }

    public init(_ items: [ReviewQueueItem]) {
        flagged = items.filter { $0.kind == .flagged }.count
        parked = items.filter { $0.kind == .parked }.count
    }
}

extension APIClient {
    /// Every open change waiting for you, across the docs you can see.
    public func reviewQueue() async throws -> [ReviewQueueItem] {
        let rows: [ReviewQueueRow] = try await get("/api/queue")
        return rows.filter(\.open).map(\.item)
    }

    /// Keep or revert (flagged), apply or discard (parked), as the person.
    public func resolveQueued(_ id: String, _ decision: ReviewQueueDecision) async throws {
        struct Body: Encodable { var annotation_id: String; var decision: String }
        struct OK: Decodable { var ok: Bool? }
        let _: OK = try await post("/api/resolve", body: Body(annotation_id: id, decision: decision.rawValue))
    }
}
