import Foundation

/// The fleet's routes (`crates/daemon/src/fabric/routes.rs`), as the app
/// uses them; a fake in tests. They answer only a signed-in person's own
/// app: a LOCAL daemon (or a server without Agent Fabric) answers 404.
public protocol FleetService: Sendable {
    func fleet() async throws -> Fleet
    func fleetStream() -> AsyncThrowingStream<SSEOutput, any Error>
    func snoozeSession(_ key: FleetSessionKey, until: Date) async throws -> FleetSession
    func unsnoozeSession(_ key: FleetSessionKey) async throws -> FleetSession
}

extension APIClient: FleetService {
    /// `GET /api/fabric/sessions`: the whole fleet, sessions in the server's
    /// attention order.
    public func fleet() async throws -> Fleet {
        try await get("/api/fabric/sessions")
    }

    /// `GET /api/fabric/runtimes`: the enrolled runtimes alone.
    public func fleetRuntimes() async throws -> [FleetRuntime] {
        struct Body: Decodable { var runtimes: [FleetRuntime] }
        let body: Body = try await get("/api/fabric/runtimes")
        return body.runtimes
    }

    /// `GET /api/fabric/stream`: one connection. It opens with the whole
    /// fleet and has no cursor, so there is nothing to resume from:
    /// reconnecting (the caller's job, see `FleetFollower`) starts over.
    public func fleetStream() -> AsyncThrowingStream<SSEOutput, any Error> {
        eventStream("/api/fabric/stream")
    }

    /// `POST /api/fabric/sessions/{runtime}/{session}/snooze`: no
    /// notification for the session before `until` (whole seconds; it must
    /// lie ahead, at most 7 days). Answers the session.
    public func snoozeSession(_ key: FleetSessionKey, until: Date) async throws -> FleetSession {
        struct Body: Encodable { var until: Int }
        return try await post(Self.snoozePath(key), body: Body(until: Int(until.timeIntervalSince1970.rounded(.up))))
    }

    /// `DELETE` on the same path: lifts the snooze. Answers the session.
    public func unsnoozeSession(_ key: FleetSessionKey) async throws -> FleetSession {
        try await send(try await request(Self.snoozePath(key), method: "DELETE"))
    }

    static func snoozePath(_ key: FleetSessionKey) -> String {
        "/api/fabric/sessions/\(key.runtimeID)/\(key.id)/snooze"
    }
}
