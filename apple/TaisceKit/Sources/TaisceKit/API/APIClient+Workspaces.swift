import Foundation

/// Workspaces (crates/daemon/src/workspaces.rs). Labels only: nothing here
/// moves or deletes a doc.
extension APIClient {
    public func workspaces() async throws -> WorkspaceList {
        try await get("/api/workspaces")
    }

    /// Names are unique case-insensitively; "Unsorted" is reserved (409/400).
    public func createWorkspace(name: String, color: String? = nil, icon: String? = nil, requestID: String = UUID().uuidString.lowercased()) async throws -> Workspace {
        try await post("/api/workspaces", body: CreateWorkspaceBody(name: name, color: color, icon: icon, requestID: requestID))
    }

    /// Rename, recolour or reorder; nil fields are kept. `sortKey` must be
    /// a valid order key (`OrderKey.between` its new neighbours).
    public func updateWorkspace(_ id: WorkspaceID, name: String? = nil, color: String? = nil, sortKey: String? = nil) async throws -> Workspace {
        struct Body: Encodable {
            var name: String?
            var color: String?
            var sort_key: String?
        }
        return try await send(body: Body(name: name, color: color, sort_key: sortKey), method: "PATCH", path: "/api/workspaces/\(id)")
    }

    /// The labels go, the docs stay (Unsorted, or an outer workspace).
    public func deleteWorkspace(_ id: WorkspaceID) async throws {
        struct Deleted: Decodable { var deleted: String }
        let _: Deleted = try await send(try await request("/api/workspaces/\(id)", method: "DELETE"))
    }

    /// Label a doc (its subtree follows); nil unlabels it.
    public func setDocWorkspace(_ doc: DocID, workspaceID: WorkspaceID?, requestID: String = UUID().uuidString.lowercased()) async throws -> WorkspaceAssignment {
        try await send(body: DocWorkspaceBody(workspaceID: workspaceID, requestID: requestID), method: "PUT", path: "/api/docs/\(doc)/workspace")
    }

    func send<T: Decodable>(body: some Encodable, method: String, path: String) async throws -> T {
        var r = try await request(path, method: method)
        r.setValue("application/json", forHTTPHeaderField: "Content-Type")
        r.httpBody = try JSONEncoder().encode(body)
        return try await send(r)
    }

    struct CreateWorkspaceBody: Encodable {
        var name: String
        var color: String?
        var icon: String?
        var requestID: String

        enum CodingKeys: String, CodingKey {
            case name, color, icon
            case requestID = "request_id"
        }
    }

    /// `workspace_id` is a required key: `null` clears the label.
    struct DocWorkspaceBody: Encodable {
        var workspaceID: WorkspaceID?
        var requestID: String

        enum CodingKeys: String, CodingKey {
            case workspaceID = "workspace_id"
            case requestID = "request_id"
        }

        func encode(to encoder: any Encoder) throws {
            var c = encoder.container(keyedBy: CodingKeys.self)
            try c.encode(workspaceID, forKey: .workspaceID)
            try c.encode(requestID, forKey: .requestID)
        }
    }
}

extension Cache {
    /// Queue a doc's workspace change (survives offline); the key is the
    /// server's `request_id`, so a replay never applies twice.
    @discardableResult
    public func enqueueDocWorkspace(_ doc: DocID, workspaceID: WorkspaceID?, key: String = UUID().uuidString.lowercased(), now: Date = .now) async throws -> OutboxEntry {
        let body = try JSONEncoder().encode(APIClient.DocWorkspaceBody(workspaceID: workspaceID, requestID: key))
        return try await enqueue(method: "PUT", path: "/api/docs/\(doc)/workspace", body: body, key: key, now: now)
    }
}
