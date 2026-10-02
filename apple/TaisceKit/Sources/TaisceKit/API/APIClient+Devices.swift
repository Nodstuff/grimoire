import Foundation

/// The APNs environment a device token belongs to: a Debug build's token
/// only works against the sandbox gateway, a Release build's against production.
public enum PushEnvironment: String, Codable, Sendable, Hashable {
    case sandbox
    case production
}

/// `POST /api/devices`: this device's APNs token, so the server can send
/// silent `content-available` pushes when something changes.
public struct DeviceRegistration: Codable, Sendable, Hashable {
    public var token: String
    public var platform: String
    public var env: PushEnvironment
    public var appVersion: String

    public init(token: String, platform: String = "ios", env: PushEnvironment, appVersion: String) {
        self.token = token
        self.platform = platform
        self.env = env
        self.appVersion = appVersion
    }

    enum CodingKeys: String, CodingKey {
        case token, platform, env
        case appVersion = "app_version"
    }
}

/// Where device registrations go: `APIClient`, or a fake in tests.
public protocol DeviceRegistry: Sendable {
    func registerDevice(_ registration: DeviceRegistration) async throws
    func unregisterDevice(token: String) async throws
}

extension APIClient: DeviceRegistry {
    struct OK: Decodable { var ok: Bool? }

    public func registerDevice(_ registration: DeviceRegistration) async throws {
        let _: OK = try await post("/api/devices", body: registration)
    }

    /// `DELETE /api/devices/{token}`. An unknown token (404) is already gone.
    public func unregisterDevice(token: String) async throws {
        let r = try await request("/api/devices/\(token)", method: "DELETE")
        do {
            let _: OK = try await send(r)
        } catch APIError.notFound {
        }
    }
}
