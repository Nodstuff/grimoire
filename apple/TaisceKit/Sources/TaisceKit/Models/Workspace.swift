import Foundation

public typealias WorkspaceID = String

/// One row of `GET /api/workspaces` (crates/daemon/src/workspaces.rs). A
/// workspace is a label on a doc, inherited by its subtree; docs resolving
/// to none are Unsorted.
public struct Workspace: Codable, Sendable, Hashable, Identifiable {
    public var id: WorkspaceID
    public var name: String
    /// `#rrggbb`, or nil for the default
    public var color: String?
    public var icon: String?
    public var sortKey: String?
    public var createdAt: String
    /// the explicitly labelled live docs (not their subtrees)
    public var docIDs: [DocID]
    /// live docs resolving to it
    public var docCount: Int

    public init(id: WorkspaceID, name: String, color: String? = nil, icon: String? = nil, sortKey: String? = nil, createdAt: String = "", docIDs: [DocID] = [], docCount: Int = 0) {
        self.id = id
        self.name = name
        self.color = color
        self.icon = icon
        self.sortKey = sortKey
        self.createdAt = createdAt
        self.docIDs = docIDs
        self.docCount = docCount
    }

    enum CodingKeys: String, CodingKey {
        case id, name, color, icon
        case sortKey = "sort_key"
        case createdAt = "created_at"
        case docIDs = "doc_ids"
        case docCount = "doc_count"
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(WorkspaceID.self, forKey: .id)
        name = try c.decode(String.self, forKey: .name)
        color = try c.decodeIfPresent(String.self, forKey: .color)
        icon = try c.decodeIfPresent(String.self, forKey: .icon)
        sortKey = try c.decodeIfPresent(String.self, forKey: .sortKey)
        createdAt = try c.decodeIfPresent(String.self, forKey: .createdAt) ?? ""
        docIDs = try c.decodeIfPresent([DocID].self, forKey: .docIDs) ?? []
        docCount = try c.decodeIfPresent(Int.self, forKey: .docCount) ?? 0
    }

    /// Sidebar order: sort key, then name.
    public static func ordered(_ all: [Workspace]) -> [Workspace] {
        all.sorted { ($0.sortKey ?? "", $0.name.lowercased()) < ($1.sortKey ?? "", $1.name.lowercased()) }
    }
}

/// `GET /api/workspaces` → `{workspaces, unsorted_count}`.
public struct WorkspaceList: Codable, Sendable, Hashable {
    public var workspaces: [Workspace]
    public var unsortedCount: Int

    public init(workspaces: [Workspace], unsortedCount: Int = 0) {
        self.workspaces = workspaces
        self.unsortedCount = unsortedCount
    }

    enum CodingKeys: String, CodingKey {
        case workspaces
        case unsortedCount = "unsorted_count"
    }
}

/// `PUT /api/docs/{id}/workspace` → `{doc_id, label, workspace_id}`.
public struct WorkspaceAssignment: Codable, Sendable, Hashable {
    public var docID: DocID
    /// the doc's own label
    public var label: WorkspaceID?
    /// what it resolves to now
    public var workspaceID: WorkspaceID?

    enum CodingKeys: String, CodingKey {
        case label
        case docID = "doc_id"
        case workspaceID = "workspace_id"
    }
}

/// Which workspace a view or call means: one workspace, or Unsorted.
public enum WorkspaceScope: Codable, Sendable, Hashable {
    case unsorted
    case id(WorkspaceID)

    /// From a resolved `workspace_id` (nil = Unsorted).
    public init(_ resolved: WorkspaceID?) {
        self = resolved.map(WorkspaceScope.id) ?? .unsorted
    }

    /// The `?workspace=` / body value.
    public var param: String {
        switch self {
        case .unsorted: "unsorted"
        case .id(let id): id
        }
    }

    /// The `workspace_id` docs resolving here carry (nil = Unsorted).
    public var workspaceID: WorkspaceID? {
        if case .id(let id) = self { return id }
        return nil
    }

    /// Stored as its `param` (an id never equals "unsorted").
    public init(param: String) {
        self = param == "unsorted" ? .unsorted : .id(param)
    }

    public init(from decoder: any Decoder) throws {
        self.init(param: try decoder.singleValueContainer().decode(String.self))
    }

    public func encode(to encoder: any Encoder) throws {
        var c = encoder.singleValueContainer()
        try c.encode(param)
    }
}
