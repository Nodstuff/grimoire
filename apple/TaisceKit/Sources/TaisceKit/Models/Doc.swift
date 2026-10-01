import Foundation

/// Doc and block ids are the server's UUIDv7 strings, kept verbatim (lowercase)
/// so cache keys and wire ids never disagree on case.
public typealias DocID = String
public typealias BlockID = String

/// One row of `GET /api/docs`: the whole doc tree is this flat list, linked by
/// `parentID` and ordered by `sortKey` among siblings.
public struct DocSummary: Codable, Sendable, Hashable, Identifiable {
    public var id: DocID
    public var parentID: DocID?
    public var title: String
    public var currentEpoch: Int
    public var sortKey: String?
    public var status: String?
    public var isCanvas: Bool
    public var isShared: Bool
    /// Set when the doc is a federation mirror ("read" / "propose" / ...).
    public var mirrorPermission: String?
    /// The workspace the doc resolves to (nil = Unsorted, or a daemon
    /// without workspaces). `/api/docs` only.
    public var workspaceID: WorkspaceID?

    public init(
        id: DocID, parentID: DocID?, title: String, currentEpoch: Int,
        sortKey: String? = nil, status: String? = nil, isCanvas: Bool = false,
        isShared: Bool = false, mirrorPermission: String? = nil, workspaceID: WorkspaceID? = nil
    ) {
        self.id = id
        self.parentID = parentID
        self.title = title
        self.currentEpoch = currentEpoch
        self.sortKey = sortKey
        self.status = status
        self.isCanvas = isCanvas
        self.isShared = isShared
        self.mirrorPermission = mirrorPermission
        self.workspaceID = workspaceID
    }

    enum CodingKeys: String, CodingKey {
        case id, title, status
        case parentID = "parent_id"
        case currentEpoch = "current_epoch"
        case sortKey = "sort_key"
        case isCanvas = "is_canvas"
        case isShared = "is_shared"
        case mirrorPermission = "mirror_permission"
        case workspaceID = "workspace_id"
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(DocID.self, forKey: .id)
        parentID = try c.decodeIfPresent(DocID.self, forKey: .parentID)
        title = try c.decode(String.self, forKey: .title)
        currentEpoch = try c.decode(Int.self, forKey: .currentEpoch)
        sortKey = try c.decodeIfPresent(String.self, forKey: .sortKey)
        status = try c.decodeIfPresent(String.self, forKey: .status)
        // decorations exist on /api/docs only, not on /api/doc/{id}'s `doc`
        isCanvas = try c.decodeIfPresent(Bool.self, forKey: .isCanvas) ?? false
        isShared = try c.decodeIfPresent(Bool.self, forKey: .isShared) ?? false
        mirrorPermission = try c.decodeIfPresent(String.self, forKey: .mirrorPermission)
        workspaceID = try c.decodeIfPresent(WorkspaceID.self, forKey: .workspaceID)
    }
}

/// The server's block types (`BlockType` in crates/store/src/types.rs).
/// Unknown future types decode as `.other` instead of failing the whole doc.
public enum BlockType: Sendable, Hashable, Codable {
    case paragraph, heading, code, diagramD2, diagramMermaid, canvasScene, comment, decision
    case other(String)

    public init(rawValue: String) {
        switch rawValue {
        case "paragraph": self = .paragraph
        case "heading": self = .heading
        case "code": self = .code
        case "diagram_d2": self = .diagramD2
        case "diagram_mermaid": self = .diagramMermaid
        case "canvas_scene": self = .canvasScene
        case "comment": self = .comment
        case "decision": self = .decision
        default: self = .other(rawValue)
        }
    }

    public var rawValue: String {
        switch self {
        case .paragraph: "paragraph"
        case .heading: "heading"
        case .code: "code"
        case .diagramD2: "diagram_d2"
        case .diagramMermaid: "diagram_mermaid"
        case .canvasScene: "canvas_scene"
        case .comment: "comment"
        case .decision: "decision"
        case .other(let s): s
        }
    }

    public init(from decoder: any Decoder) throws {
        self.init(rawValue: try decoder.singleValueContainer().decode(String.self))
    }

    public func encode(to encoder: any Encoder) throws {
        var c = encoder.singleValueContainer()
        try c.encode(rawValue)
    }
}

/// A block: one markdown unit. Headings own the blocks of their section as
/// children; `content` is the block's own markdown (fences included).
public struct Block: Codable, Sendable, Hashable, Identifiable {
    public var id: BlockID
    public var docID: DocID
    public var parentID: BlockID?
    public var orderKey: String
    public var blockType: BlockType
    public var content: String
    public var epoch: Int
    public var deleted: Bool
    /// Comment blocks: the content block the thread anchors to.
    public var refersTo: BlockID?

    public init(
        id: BlockID, docID: DocID, parentID: BlockID?, orderKey: String,
        blockType: BlockType, content: String, epoch: Int = 0,
        deleted: Bool = false, refersTo: BlockID? = nil
    ) {
        self.id = id
        self.docID = docID
        self.parentID = parentID
        self.orderKey = orderKey
        self.blockType = blockType
        self.content = content
        self.epoch = epoch
        self.deleted = deleted
        self.refersTo = refersTo
    }

    enum CodingKeys: String, CodingKey {
        case id, content, epoch, deleted
        case docID = "doc_id"
        case parentID = "parent_id"
        case orderKey = "order_key"
        case blockType = "block_type"
        case refersTo = "refers_to"
    }
}

/// `{block, children}` — children already ordered by `order_key`.
public struct BlockNode: Codable, Sendable, Hashable {
    public var block: Block
    public var children: [BlockNode]

    public init(block: Block, children: [BlockNode] = []) {
        self.block = block
        self.children = children
    }
}

/// `GET /api/doc/{id}` → `{doc, roots}`.
public struct DocTree: Codable, Sendable, Hashable {
    public var doc: DocSummary
    public var roots: [BlockNode]

    public init(doc: DocSummary, roots: [BlockNode]) {
        self.doc = doc
        self.roots = roots
    }

    /// Depth-first, document order, with each block's nesting depth.
    public func flattened() -> [(block: Block, depth: Int)] {
        var out: [(Block, Int)] = []
        func walk(_ nodes: [BlockNode], _ depth: Int) {
            for n in nodes {
                out.append((n.block, depth))
                walk(n.children, depth + 1)
            }
        }
        walk(roots, 0)
        return out
    }
}

/// `GET /api/search` → `[SearchHit]`.
public struct SearchHit: Codable, Sendable, Hashable {
    public var block: Block
    public var docTitle: String

    enum CodingKeys: String, CodingKey {
        case block
        case docTitle = "doc_title"
    }
}
