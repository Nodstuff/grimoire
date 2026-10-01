import Foundation

/// One entry of the global change log (`/api/changes`, `/api/changes/stream`).
public struct Change: Codable, Sendable, Hashable {
    public enum Kind: Sendable, Hashable, Codable {
        /// the doc's blocks changed (epoch bumped)
        case doc
        /// the doc tree changed: create, rename, move, status
        case tree
        case deleted
        case restored
        case other(String)

        public init(from decoder: any Decoder) throws {
            switch try decoder.singleValueContainer().decode(String.self) {
            case "doc": self = .doc
            case "tree": self = .tree
            case "deleted": self = .deleted
            case "restored": self = .restored
            case let s: self = .other(s)
            }
        }

        public func encode(to encoder: any Encoder) throws {
            var c = encoder.singleValueContainer()
            switch self {
            case .doc: try c.encode("doc")
            case .tree: try c.encode("tree")
            case .deleted: try c.encode("deleted")
            case .restored: try c.encode("restored")
            case .other(let s): try c.encode(s)
            }
        }
    }

    public var seq: Int
    public var docID: DocID
    public var kind: Kind
    public var epoch: Int?
    public var at: String

    public init(seq: Int, docID: DocID, kind: Kind, epoch: Int? = nil, at: String = "") {
        self.seq = seq
        self.docID = docID
        self.kind = kind
        self.epoch = epoch
        self.at = at
    }

    enum CodingKeys: String, CodingKey {
        case seq, kind, epoch, at
        case docID = "doc_id"
    }
}

/// `GET /api/changes?since=&limit=` → one page.
public struct ChangePage: Codable, Sendable, Hashable {
    /// the cursor to pass as `since` next
    public var seq: Int
    public var changes: [Change]
    public var more: Bool

    public init(seq: Int, changes: [Change], more: Bool) {
        self.seq = seq
        self.changes = changes
        self.more = more
    }
}
