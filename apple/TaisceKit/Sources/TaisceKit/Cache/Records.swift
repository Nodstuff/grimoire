import Foundation
import GRDB

/// A doc-tree row. `bodyEpoch` is the epoch of the cached blocks (nil = body
/// never fetched); `bodyEpoch < currentEpoch` means the body is stale.
public struct DocRecord: Codable, Sendable, Hashable, Identifiable, FetchableRecord, PersistableRecord {
    public static let databaseTableName = "docs"

    public var id: DocID
    public var parentID: DocID?
    public var title: String
    public var currentEpoch: Int
    public var sortKey: String?
    public var status: String?
    public var isCanvas: Bool
    public var isShared: Bool
    public var mirrorPermission: String?
    public var bodyEpoch: Int?

    public var isBodyStale: Bool { bodyEpoch.map { $0 < currentEpoch } ?? true }

    public enum CodingKeys: String, CodingKey, ColumnExpression {
        case id, title, status
        case parentID = "parent_id"
        case currentEpoch = "current_epoch"
        case sortKey = "sort_key"
        case isCanvas = "is_canvas"
        case isShared = "is_shared"
        case mirrorPermission = "mirror_permission"
        case bodyEpoch = "body_epoch"
    }

    public typealias Columns = CodingKeys

    init(_ s: DocSummary, bodyEpoch: Int?) {
        id = s.id
        parentID = s.parentID
        title = s.title
        currentEpoch = s.currentEpoch
        sortKey = s.sortKey
        status = s.status
        isCanvas = s.isCanvas
        isShared = s.isShared
        mirrorPermission = s.mirrorPermission
        self.bodyEpoch = bodyEpoch
    }

    public var summary: DocSummary {
        DocSummary(
            id: id, parentID: parentID, title: title, currentEpoch: currentEpoch,
            sortKey: sortKey, status: status, isCanvas: isCanvas, isShared: isShared,
            mirrorPermission: mirrorPermission
        )
    }
}

/// A cached block, flattened in document order (`position`) with its depth,
/// so a doc renders from one ordered query.
public struct BlockRecord: Codable, Sendable, Hashable, Identifiable, FetchableRecord, PersistableRecord {
    public static let databaseTableName = "blocks"

    public var id: BlockID
    public var docID: DocID
    public var parentID: BlockID?
    public var orderKey: String
    public var blockType: String
    public var content: String
    public var epoch: Int
    public var refersTo: BlockID?
    public var position: Int
    public var depth: Int

    public enum CodingKeys: String, CodingKey, ColumnExpression {
        case id, content, epoch, position, depth
        case docID = "doc_id"
        case parentID = "parent_id"
        case orderKey = "order_key"
        case blockType = "block_type"
        case refersTo = "refers_to"
    }

    public typealias Columns = CodingKeys

    public var block: Block {
        Block(
            id: id, docID: docID, parentID: parentID, orderKey: orderKey,
            blockType: BlockType(rawValue: blockType), content: content, epoch: epoch,
            refersTo: refersTo
        )
    }
}

/// A to-do parsed out of the cached To-do doc (one row per item per day).
public struct TodoRecord: Codable, Sendable, Hashable, FetchableRecord, PersistableRecord {
    public static let databaseTableName = "todos"

    public var date: String
    public var position: Int
    /// ' ' open, 'x' done, '>' carried forward
    public var mark: String
    public var text: String
    public var deadline: String?
    public var carriedFrom: String?
    public var note: String?

    public var isOpen: Bool { mark == " " }
    /// `deadline` is the stored token: `YYYY-MM-DD`, `YYYY-MM-DDTHH:MMZ`, or legacy `D HH:MM`
    public var deadlineValue: Deadline? { deadline.flatMap { Deadline(stored: $0) } }
    /// The deadline as a wall-clock date/time in the device's zone.
    public var due: Due? { deadlineValue?.due() }
    public func isOverdue(now: Date = .now, in timeZone: TimeZone = .current) -> Bool {
        isOpen && (deadlineValue?.isOverdue(now: now, in: timeZone) ?? false)
    }

    public enum CodingKeys: String, CodingKey, ColumnExpression {
        case date, position, mark, text, deadline, note
        case carriedFrom = "carried_from"
    }

    public typealias Columns = CodingKeys
}
