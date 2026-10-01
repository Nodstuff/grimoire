import Foundation

/// An APNs device token as the server wants it: lowercase hex, two digits a byte.
public enum PushToken {
    public static func hex(_ token: Data) -> String {
        let digits = Array("0123456789abcdef".utf8)
        var out = [UInt8]()
        out.reserveCapacity(token.count * 2)
        for byte in token {
            out.append(digits[Int(byte >> 4)])
            out.append(digits[Int(byte & 0x0f)])
        }
        return String(decoding: out, as: UTF8.self)
    }
}

/// What was last sent to which server, so a token goes out when it (or
/// anything else in the registration) changes, and otherwise at most daily.
public struct SentPushRegistration: Codable, Sendable, Hashable {
    public var server: String
    public var registration: DeviceRegistration
    public var sentAt: Date

    public init(server: String, registration: DeviceRegistration, sentAt: Date) {
        self.server = server
        self.registration = registration
        self.sentAt = sentAt
    }

    /// The server forgets nothing on its own, but a daily re-send keeps its
    /// row fresh (and repairs one lost to a server-side reset).
    public static let resendInterval: TimeInterval = 24 * 60 * 60

    /// Send `registration` to `server`? Yes when nothing was sent, when the
    /// server, token, environment or app version differ, or after a day.
    public static func shouldSend(_ registration: DeviceRegistration, to server: String, last: SentPushRegistration?, now: Date) -> Bool {
        guard let last, last.server == server, last.registration == registration else { return true }
        let age = now.timeIntervalSince(last.sentAt)
        // a clock set backwards counts as stale too
        return age >= resendInterval || age < 0
    }
}

/// Where the last sent registration is kept between launches.
public protocol PushRegistrationStore: Sendable {
    func load() -> SentPushRegistration?
    func save(_ sent: SentPushRegistration?)
}

/// `UserDefaults` (thread-safe; not marked `Sendable` in the SDK).
public struct UserDefaultsPushStore: PushRegistrationStore, @unchecked Sendable {
    public static let defaultKey = "pushRegistration"
    let defaults: UserDefaults
    let key: String

    public init(defaults: UserDefaults = .standard, key: String = Self.defaultKey) {
        self.defaults = defaults
        self.key = key
    }

    public func load() -> SentPushRegistration? {
        defaults.data(forKey: key).flatMap { try? JSONDecoder().decode(SentPushRegistration.self, from: $0) }
    }

    public func save(_ sent: SentPushRegistration?) {
        if let sent, let data = try? JSONEncoder().encode(sent) {
            defaults.set(data, forKey: key)
        } else {
            defaults.removeObject(forKey: key)
        }
    }
}

/// Keeps the server's copy of this device's APNs token current: the app
/// hands it the token whenever iOS delivers one (every launch, after
/// `registerForRemoteNotifications`) and the signed-in server's registry;
/// it sends when the token changes and at most daily otherwise, and
/// deletes the registration on sign-out.
public actor PushRegistrar {
    public enum Outcome: Equatable, Sendable {
        /// sent to the server
        case sent
        /// the server already has it (sent within the day)
        case upToDate
        /// no token yet, or no signed-in server to send it to
        case waiting
        case failed(String)
    }

    private let store: any PushRegistrationStore
    private let environment: PushEnvironment
    private let appVersion: String
    private let now: @Sendable () -> Date
    private var token: String?
    private var server: String?
    private var registry: (any DeviceRegistry)?

    public init(store: any PushRegistrationStore, environment: PushEnvironment, appVersion: String, now: @escaping @Sendable () -> Date = { .now }) {
        self.store = store
        self.environment = environment
        self.appVersion = appVersion
        self.now = now
    }

    /// The token iOS delivered, as raw bytes.
    @discardableResult
    public func didRegister(deviceToken: Data) async -> Outcome {
        await didRegister(token: PushToken.hex(deviceToken))
    }

    @discardableResult
    public func didRegister(token: String) async -> Outcome {
        self.token = token
        return await sendIfNeeded()
    }

    /// A signed-in server (SERVER mode): registrations go to `registry`.
    @discardableResult
    public func connect(server: String, registry: any DeviceRegistry) async -> Outcome {
        self.server = server
        self.registry = registry
        return await sendIfNeeded()
    }

    /// Stop sending (signed out, or a server without sign-in).
    public func disconnect() {
        server = nil
        registry = nil
    }

    /// Send the registration if the server lacks it or it is a day old.
    @discardableResult
    public func sendIfNeeded() async -> Outcome {
        guard let token, let server, let registry else { return .waiting }
        let registration = DeviceRegistration(token: token, env: environment, appVersion: appVersion)
        let at = now()
        guard SentPushRegistration.shouldSend(registration, to: server, last: store.load(), now: at) else { return .upToDate }
        do {
            try await registry.registerDevice(registration)
        } catch {
            return .failed(String(describing: error))
        }
        // the token may have moved on while the request was out: keep the
        // record only if it still describes what we hold
        if self.token == token, self.server == server {
            store.save(SentPushRegistration(server: server, registration: registration, sentAt: at))
        }
        return .sent
    }

    /// Sign-out: delete the registration on the server it was sent to
    /// (while the bearer is still good), then forget it locally either way,
    /// so the next sign-in sends afresh. Best effort: a failed delete only
    /// leaves the server pushing to a device that will ignore it.
    @discardableResult
    public func unregister() async -> Outcome {
        let last = store.load()
        let (registry, server) = (registry, server)
        store.save(nil)
        disconnect()
        // only the server it went to can delete it (another one's bearer won't)
        guard let last, let registry, last.server == server else { return .waiting }
        do {
            try await registry.unregisterDevice(token: last.registration.token)
            return .sent
        } catch {
            return .failed(String(describing: error))
        }
    }
}
