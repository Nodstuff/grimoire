import Foundation

extension Cache {
    /// Queue ticking a to-do done (or open). Same body as
    /// `APIClient.todoToggle`.
    @discardableResult
    public func enqueueToggle(date: String, itemID: String, done: Bool, key: String = UUID().uuidString.lowercased(), now: Date = .now) async throws -> OutboxEntry {
        struct Body: Encodable { var date: String; var item_id: String; var done: Bool }
        let body = try JSONEncoder().encode(Body(date: date, item_id: itemID, done: done))
        return try await enqueue(method: "POST", path: "/api/todo/toggle", body: body, key: key, now: now)
    }
}
