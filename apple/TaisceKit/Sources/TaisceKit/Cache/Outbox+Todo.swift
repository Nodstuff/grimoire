import Foundation

/// Typed, queued to-do writes: callers never spell a route or a body. Each
/// carries the device's `TodoClock` (`today` + `utc_offset`).
extension Cache {
    /// Tick or untick an item (`POST /api/todo/toggle`).
    @discardableResult
    public func enqueueTodoToggle(date: String, itemID: String, done: Bool, clock: TodoClock = TodoClock(), key: String = UUID().uuidString.lowercased(), now: Date = .now) async throws -> OutboxEntry {
        try await enqueueTodo("/api/todo/toggle", date: date, itemID: itemID, done: done, clock: clock, key: key, now: now)
    }

    /// A new item on `date`; the server reads any `due …` phrase in `text`
    /// against `clock` (`POST /api/todo`).
    @discardableResult
    public func enqueueTodoAdd(date: String, text: String, clock: TodoClock = TodoClock(), key: String = UUID().uuidString.lowercased(), now: Date = .now) async throws -> OutboxEntry {
        try await enqueueTodo("/api/todo", date: date, text: text, clock: clock, key: key, now: now)
    }
}
