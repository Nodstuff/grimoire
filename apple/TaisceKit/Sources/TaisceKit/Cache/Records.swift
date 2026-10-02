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
    /// resolved workspace from `/api/docs` or a change row (nil = Unsorted)
    public var workspaceID: WorkspaceID?

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
        case workspaceID = "workspace_id"
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
        workspaceID = s.workspaceID
    }

    public var summary: DocSummary {
        DocSummary(
            id: id, parentID: parentID, title: title, currentEpoch: currentEpoch,
            sortKey: sortKey, status: status, isCanvas: isCanvas, isShared: isShared,
            mirrorPermission: mirrorPermission, workspaceID: workspaceID
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
    /// the To-do doc it was parsed from (one list per workspace)
    public var docID: DocID = ""

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
        case docID = "doc_id"
    }

    public typealias Columns = CodingKeys
}

/// A cached `GET /api/workspaces` row (doc ids and counts stay server-side:
/// a doc's workspace comes from its own `workspace_id`).
public struct WorkspaceRecord: Codable, Sendable, Hashable, FetchableRecord, PersistableRecord {
    public static let databaseTableName = "workspaces"

    public var id: WorkspaceID
    public var name: String
    public var color: String?
    public var icon: String?
    public var sortKey: String?
    public var docCount: Int
    // multi-user (v4): kept so a viewer stays read-only offline
    public var role: String?
    public var ownerID: String?
    public var ownerName: String?
    public var displayName: String?
    public var shared: Bool

    public enum CodingKeys: String, CodingKey, ColumnExpression {
        case id, name, color, icon, role, shared
        case sortKey = "sort_key"
        case docCount = "doc_count"
        case ownerID = "owner_id"
        case ownerName = "owner_name"
        case displayName = "display_name"
    }

    init(_ w: Workspace) {
        id = w.id
        name = w.name
        color = w.color
        icon = w.icon
        sortKey = w.sortKey
        docCount = w.docCount
        role = w.role
        ownerID = w.ownerID
        ownerName = w.ownerName
        displayName = w.displayName
        shared = w.shared
    }

    public var workspace: Workspace {
        Workspace(
            id: id, name: name, color: color, icon: icon, sortKey: sortKey, docCount: docCount,
            role: role, ownerID: ownerID, ownerName: ownerName, displayName: displayName, shared: shared
        )
    }
}
