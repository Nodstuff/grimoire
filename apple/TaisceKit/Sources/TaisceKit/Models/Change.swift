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

    /// The doc as it stands when the page is SERVED (not as of `seq`): an
    /// older row can carry a newer title. Nil only for a hard-deleted doc.
    public struct DocState: Codable, Sendable, Hashable {
        public var title: String
        public var parentID: DocID?
        public var sortKey: String?
        public var status: String?
        public var currentEpoch: Int
        public var deleted: Bool
        /// resolved workspace (nil = Unsorted, or a daemon without workspaces)
        public var workspaceID: WorkspaceID?

        public init(title: String, parentID: DocID? = nil, sortKey: String? = nil, status: String? = nil, currentEpoch: Int = 0, deleted: Bool = false, workspaceID: WorkspaceID? = nil) {
            self.title = title
            self.parentID = parentID
            self.sortKey = sortKey
            self.status = status
            self.currentEpoch = currentEpoch
            self.deleted = deleted
            self.workspaceID = workspaceID
        }

        enum CodingKeys: String, CodingKey {
            case title, status, deleted
            case parentID = "parent_id"
            case sortKey = "sort_key"
            case currentEpoch = "current_epoch"
            case workspaceID = "workspace_id"
        }
    }

    public var seq: Int
    public var docID: DocID
    public var kind: Kind
    public var epoch: Int?
    public var at: String
    public var doc: DocState?

    public init(seq: Int, docID: DocID, kind: Kind, epoch: Int? = nil, at: String = "", doc: DocState? = nil) {
        self.seq = seq
        self.docID = docID
        self.kind = kind
        self.epoch = epoch
        self.at = at
        self.doc = doc
    }

    enum CodingKeys: String, CodingKey {
        case seq, kind, epoch, at, doc
        case docID = "doc_id"
    }
}

/// `GET /api/changes?since=&limit=` → one page.
public struct ChangePage: Codable, Sendable, Hashable {
    /// The journal HEAD at serving time, not this page's last row. While
    /// `more` is true, page on from `changes.last.seq`; jumping to `seq`
    /// would skip the rows in between. A head below your cursor means the
    /// server's database was reset.
    public var seq: Int
    public var changes: [Change]
    public var more: Bool

    public init(seq: Int, changes: [Change], more: Bool) {
        self.seq = seq
        self.changes = changes
        self.more = more
    }
}
