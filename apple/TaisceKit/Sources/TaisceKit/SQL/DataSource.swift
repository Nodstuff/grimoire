import Foundation
import Synchronization
import Valet

public enum DataSourceKind: String, Codable, Sendable, Hashable, CaseIterable, Identifiable {
    case sqlite, postgres, clickhouse

    public var id: String { rawValue }

    public var label: String {
        switch self {
        case .sqlite: "SQLite"
        case .postgres: "Postgres"
        case .clickhouse: "ClickHouse"
        }
    }

    public var hasPassword: Bool { self != .sqlite }
}

/// Postgres TLS, named as libpq's `sslmode`: `prefer` and `require`
/// encrypt without checking the certificate, `verifyFull` checks it and
/// the host name.
public enum PostgresTLSMode: String, Codable, Sendable, Hashable, CaseIterable, Identifiable {
    case disable, prefer, require, verifyFull = "verify-full"

    public var id: String { rawValue }

    public var label: String {
        switch self {
        case .disable: "Off"
        case .prefer: "Prefer (unverified)"
        case .require: "Require (unverified)"
        case .verifyFull: "Require and verify"
        }
    }
}

/// A database this Mac can query from SQL blocks. Lives in Application
/// Support as JSON, never synced; the password is in the Keychain under the
/// source's id.
public struct DataSource: Codable, Sendable, Hashable, Identifiable {
    public var id: String
    /// what ```` ```sql db=<name> ```` says: unique on this Mac
    public var name: String
    public var kind: DataSourceKind
    /// SQLite: the database file (`~` allowed)
    public var path: String = ""
    /// Postgres
    public var host: String = ""
    public var port: Int = 5432
    public var database: String = ""
    public var user: String = ""
    public var tlsMode: PostgresTLSMode = .prefer
    /// ClickHouse: the HTTP(S) interface, e.g. https://host:8443
    public var url: String = ""
    /// off: the database itself refuses writes (read-only open, a
    /// read-only session or setting)
    public var allowWrites = false

    public init(id: String = UUID().uuidString, name: String, kind: DataSourceKind) {
        self.id = id
        self.name = name
        self.kind = kind
        if kind == .clickhouse { user = "default" }
    }

    enum CodingKeys: String, CodingKey {
        case id, name, kind, path, host, port, database, user, tlsMode, url, allowWrites
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        name = try c.decode(String.self, forKey: .name)
        kind = try c.decode(DataSourceKind.self, forKey: .kind)
        path = try c.decodeIfPresent(String.self, forKey: .path) ?? ""
        host = try c.decodeIfPresent(String.self, forKey: .host) ?? ""
        port = try c.decodeIfPresent(Int.self, forKey: .port) ?? 5432
        database = try c.decodeIfPresent(String.self, forKey: .database) ?? ""
        user = try c.decodeIfPresent(String.self, forKey: .user) ?? ""
        tlsMode = try c.decodeIfPresent(PostgresTLSMode.self, forKey: .tlsMode) ?? .prefer
        url = try c.decodeIfPresent(String.self, forKey: .url) ?? ""
        allowWrites = try c.decodeIfPresent(Bool.self, forKey: .allowWrites) ?? false
    }

    /// A one-line "where": the file, host/db or URL.
    public var summary: String {
        switch kind {
        case .sqlite: path
        case .postgres: "\(user.isEmpty ? "" : user + "@")\(host):\(port)/\(database)"
        case .clickhouse: url + (database.isEmpty ? "" : " · \(database)")
        }
    }

    /// Why this source can't be saved as is (nil = fine). `others` are the
    /// rest of this Mac's sources, for the unique name.
    public func problem(among others: [DataSource]) -> String? {
        let n = name.trimmingCharacters(in: .whitespaces)
        if n.isEmpty { return "Give it a name." }
        if !Self.isValidName(n) { return "A name is letters, digits, - _ and . only (it goes in db=\(n))." }
        if others.contains(where: { $0.id != id && $0.name.caseInsensitiveCompare(n) == .orderedSame }) {
            return "There is already a data source named \(n)."
        }
        switch kind {
        case .sqlite:
            if path.trimmingCharacters(in: .whitespaces).isEmpty { return "Choose the database file." }
        case .postgres:
            if host.trimmingCharacters(in: .whitespaces).isEmpty { return "Enter the host." }
            if !(1...65535).contains(port) { return "The port is 1-65535." }
            if user.trimmingCharacters(in: .whitespaces).isEmpty { return "Enter the user." }
        case .clickhouse:
            guard let u = URL(string: url.trimmingCharacters(in: .whitespaces)), let s = u.scheme?.lowercased(), s == "http" || s == "https", u.host() != nil else {
                return "Enter the HTTP interface's URL, e.g. https://host:8443."
            }
        }
        return nil
    }

    public static func isValidName(_ s: String) -> Bool {
        !s.isEmpty && s.unicodeScalars.allSatisfy { CharacterSet.alphanumerics.contains($0) || "-_.".unicodeScalars.contains($0) }
    }
}

/// This Mac's data sources, in a JSON file. Not thread-safe by itself: the
/// app's model owns one and calls it from the main actor.
public struct DataSourceFile: Sendable {
    public let url: URL

    public init(url: URL) {
        self.url = url
    }

    struct Contents: Codable {
        var version = 1
        var sources: [DataSource]
    }

    /// The saved sources, sorted by name; none when the file doesn't exist.
    public func load() throws -> [DataSource] {
        guard FileManager.default.fileExists(atPath: url.path) else { return [] }
        let data = try Data(contentsOf: url)
        return try JSONDecoder().decode(Contents.self, from: data).sources
            .sorted { $0.name.localizedStandardCompare($1.name) == .orderedAscending }
    }

    public func save(_ sources: [DataSource]) throws {
        try FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        let enc = JSONEncoder()
        enc.outputFormatting = [.prettyPrinted, .sortedKeys]
        try enc.encode(Contents(sources: sources)).write(to: url, options: [.atomic])
    }
}

/// Where data-source passwords live, by source id.
public protocol DataSourceSecrets: Sendable {
    func password(for id: String) throws -> String?
    func setPassword(_ password: String?, for id: String) throws
}

/// The Keychain (through Valet), this device only, never synced.
public struct KeychainDataSourceSecrets: DataSourceSecrets {
    public static let defaultIdentifier = "ie.null.taisce.datasource"
    let valet: Valet

    public init(identifier: String = Self.defaultIdentifier) throws {
        guard let id = Identifier(nonEmpty: identifier) else { throw KeychainTokenStore.EmptyIdentifier() }
        valet = .valet(with: id, accessibility: .afterFirstUnlockThisDeviceOnly)
    }

    public func password(for id: String) throws -> String? {
        do {
            return try valet.string(forKey: id)
        } catch .itemNotFound {
            return nil
        }
    }

    public func setPassword(_ password: String?, for id: String) throws {
        if let password, !password.isEmpty {
            try valet.setString(password, forKey: id)
        } else {
            do { try valet.removeObject(forKey: id) } catch .itemNotFound {}
        }
    }

    /// Every password under this identifier (tests' cleanup).
    public func removeAll() throws {
        try valet.removeAllObjects()
    }
}

public final class MemoryDataSourceSecrets: DataSourceSecrets {
    private let state = Mutex<[String: String]>([:])
    public init(_ initial: [String: String] = [:]) { state.withLock { $0 = initial } }
    public func password(for id: String) throws -> String? { state.withLock { $0[id] } }
    public func setPassword(_ password: String?, for id: String) throws {
        state.withLock { $0[id] = (password?.isEmpty ?? true) ? nil : password }
    }
}
