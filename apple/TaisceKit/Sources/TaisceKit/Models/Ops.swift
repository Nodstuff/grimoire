import Foundation

/// The block ops `POST /api/propose` takes (`OpKind`, tagged by `op`).
/// Doc-level ops (rename/move/status/delete) go through their own routes.
public enum BlockOp: Sendable, Hashable, Codable {
    case insert(blockID: BlockID?, parentID: BlockID?, orderKey: String, type: BlockType, content: String)
    case replace(target: BlockID, content: String)
    case delete(target: BlockID)
    case move(target: BlockID, newParent: BlockID?, newOrderKey: String)

    private enum Keys: String, CodingKey {
        case op, target, content
        case blockID = "block_id"
        case parentID = "parent_id"
        case orderKey = "order_key"
        case blockType = "block_type"
        case newParent = "new_parent"
        case newOrderKey = "new_order_key"
    }

    /// Read back from the outbox (`DocEditor` overlays queued edits).
    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: Keys.self)
        switch try c.decode(String.self, forKey: .op) {
        case "insert":
            self = .insert(
                blockID: try c.decodeIfPresent(BlockID.self, forKey: .blockID),
                parentID: try c.decodeIfPresent(BlockID.self, forKey: .parentID),
                orderKey: try c.decode(String.self, forKey: .orderKey),
                type: try c.decode(BlockType.self, forKey: .blockType),
                content: try c.decode(String.self, forKey: .content)
            )
        case "replace":
            self = .replace(target: try c.decode(BlockID.self, forKey: .target), content: try c.decode(String.self, forKey: .content))
        case "delete":
            self = .delete(target: try c.decode(BlockID.self, forKey: .target))
        case "move":
            self = .move(
                target: try c.decode(BlockID.self, forKey: .target),
                newParent: try c.decodeIfPresent(BlockID.self, forKey: .newParent),
                newOrderKey: try c.decode(String.self, forKey: .newOrderKey)
            )
        case let op:
            throw DecodingError.dataCorruptedError(forKey: .op, in: c, debugDescription: "not a block op: \(op)")
        }
    }

    public func encode(to encoder: any Encoder) throws {
        var c = encoder.container(keyedBy: Keys.self)
        switch self {
        case let .insert(blockID, parentID, orderKey, type, content):
            try c.encode("insert", forKey: .op)
            // absent → the server mints a UUIDv7
            try c.encodeIfPresent(blockID, forKey: .blockID)
            try c.encode(parentID, forKey: .parentID)
            try c.encode(orderKey, forKey: .orderKey)
            try c.encode(type, forKey: .blockType)
            try c.encode(content, forKey: .content)
        case let .replace(target, content):
            try c.encode("replace", forKey: .op)
            try c.encode(target, forKey: .target)
            try c.encode(content, forKey: .content)
        case let .delete(target):
            try c.encode("delete", forKey: .op)
            try c.encode(target, forKey: .target)
        case let .move(target, newParent, newOrderKey):
            try c.encode("move", forKey: .op)
            try c.encode(target, forKey: .target)
            try c.encode(newParent, forKey: .newParent)
            try c.encode(newOrderKey, forKey: .newOrderKey)
        }
    }
}

/// `POST /api/propose` body. `requestID` makes a retry idempotent per principal.
public struct ProposeRequest: Codable, Sendable, Hashable {
    public struct Op: Codable, Sendable, Hashable {
        public var kind: BlockOp
        public var sourceRefs: [String]

        enum CodingKeys: String, CodingKey {
            case kind
            case sourceRefs = "source_refs"
        }
    }

    public var docID: DocID
    public var baseEpoch: Int
    public var ops: [Op]
    public var requestID: String?

    public init(docID: DocID, baseEpoch: Int, ops: [BlockOp], requestID: String? = nil) {
        self.docID = docID
        self.baseEpoch = baseEpoch
        self.ops = ops.map { Op(kind: $0, sourceRefs: []) }
        self.requestID = requestID
    }

    enum CodingKeys: String, CodingKey {
        case ops
        case docID = "doc_id"
        case baseEpoch = "base_epoch"
        case requestID = "request_id"
    }
}

/// `POST /api/propose_markdown` body: the doc's complete new markdown.
public struct ProposeMarkdownRequest: Encodable, Sendable, Hashable {
    public var docID: DocID
    public var baseEpoch: Int
    public var markdown: String
    public var requestID: String?

    public init(docID: DocID, baseEpoch: Int, markdown: String, requestID: String? = nil) {
        self.docID = docID
        self.baseEpoch = baseEpoch
        self.markdown = markdown
        self.requestID = requestID
    }

    enum CodingKeys: String, CodingKey {
        case markdown
        case docID = "doc_id"
        case baseEpoch = "base_epoch"
        case requestID = "request_id"
    }
}

public enum Verdict: String, Codable, Sendable, Hashable {
    case green, yellow, red
}

/// Per-op outcome; `applied == false` on red (parked for a human).
public struct ProposeVerdict: Codable, Sendable, Hashable {
    public var opID: String
    public var blockID: BlockID?
    public var verdict: Verdict
    public var confidence: Double
    public var applied: Bool
    public var note: String

    enum CodingKeys: String, CodingKey {
        case verdict, confidence, applied, note
        case opID = "op_id"
        case blockID = "block_id"
    }
}

public struct ProposeOutcome: Codable, Sendable, Hashable {
    public var docID: DocID
    public var epoch: Int
    public var verdicts: [ProposeVerdict]

    enum CodingKeys: String, CodingKey {
        case epoch, verdicts
        case docID = "doc_id"
    }
}
