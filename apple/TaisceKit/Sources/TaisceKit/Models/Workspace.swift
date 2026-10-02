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
    // ADR 0004 (multi-user servers); all absent on older servers
    /// the signed-in user's role in it: "owner", "editor" or "viewer"
    public var role: String?
    public var ownerID: String?
    /// the owner's display name
    public var ownerName: String?
    /// what a switcher shows: the name, or "Work · Aoife" when someone
    /// else's workspace clashes with another visible one
    public var displayName: String?
    /// it has more than one member
    public var shared: Bool

    public init(id: WorkspaceID, name: String, color: String? = nil, icon: String? = nil, sortKey: String? = nil, createdAt: String = "", docIDs: [DocID] = [], docCount: Int = 0, role: String? = nil, ownerID: String? = nil, ownerName: String? = nil, displayName: String? = nil, shared: Bool = false) {
        self.id = id
        self.name = name
        self.color = color
        self.icon = icon
        self.sortKey = sortKey
        self.createdAt = createdAt
        self.docIDs = docIDs
        self.docCount = docCount
        self.role = role
        self.ownerID = ownerID
        self.ownerName = ownerName
        self.displayName = displayName
        self.shared = shared
    }

    enum CodingKeys: String, CodingKey {
        case id, name, color, icon, role, shared
        case sortKey = "sort_key"
        case createdAt = "created_at"
        case docIDs = "doc_ids"
        case docCount = "doc_count"
        case ownerID = "owner_id"
        case ownerName = "owner_name"
        case displayName = "display_name"
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
        role = try c.decodeIfPresent(String.self, forKey: .role)
        ownerID = try c.decodeIfPresent(String.self, forKey: .ownerID)
        ownerName = try c.decodeIfPresent(String.self, forKey: .ownerName)
        displayName = try c.decodeIfPresent(String.self, forKey: .displayName)
        shared = try c.decodeIfPresent(Bool.self, forKey: .shared) ?? false
    }

    /// What a switcher shows: `display_name`, else (older servers) the name.
    public var label: String {
        guard let displayName, !displayName.isEmpty else { return name }
        return displayName
    }

    /// The signed-in user owns it (an older server, without roles, means yes).
    public var isOwn: Bool { role == nil || role == "owner" }

    /// Read-only for the signed-in user.
    public var isViewOnly: Bool { role == "viewer" }

    /// Sidebar order: your own workspaces, then the ones shared with you;
    /// each by sort key, then name.
    public static func ordered(_ all: [Workspace]) -> [Workspace] {
        all.sorted {
            ($0.isOwn ? 0 : 1, $0.sortKey ?? "", $0.name.lowercased(), $0.id) < ($1.isOwn ? 0 : 1, $1.sortKey ?? "", $1.name.lowercased(), $1.id)
        }
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
