import Foundation
import Observation
import TaisceKit
#if targetEnvironment(macCatalyst)
import TaisceSQLPostgres
#endif

/// This Mac's SQL data sources: the list in Application Support
/// (`datasources.json`), passwords in the Keychain by source id. Never
/// synced, never sent to the server; kept across sign-outs and server
/// switches (they belong to the Mac, not the account).
@MainActor @Observable
final class DataSourceStore {
    private(set) var sources: [DataSource] = []
    /// the file couldn't be read (shown in Settings)
    private(set) var loadError: String?
    @ObservationIgnored let file: DataSourceFile?
    @ObservationIgnored private var secretsStore: (any DataSourceSecrets)?
    @ObservationIgnored private let makeSecrets: () throws -> any DataSourceSecrets

    init(
        file: DataSourceFile? = AppPaths.dataSourcesFile.map(DataSourceFile.init(url:)),
        secrets: @escaping () throws -> any DataSourceSecrets = { try KeychainDataSourceSecrets(identifier: AppPaths.dataSourceKeychainIdentifier) }
    ) {
        self.file = file
        self.makeSecrets = secrets
        reload()
    }

    func reload() {
        guard let file else { return }
        do {
            sources = try file.load()
            loadError = nil
        } catch {
            loadError = "Couldn't read \(file.url.lastPathComponent): \(error.localizedDescription)"
        }
    }

    /// The Keychain, opened on first use (not at launch, not on the iPhone).
    private func secrets() throws -> any DataSourceSecrets {
        if let secretsStore { return secretsStore }
        let s = try makeSecrets()
        secretsStore = s
        return s
    }

    /// `db=<name>`: names match ignoring case.
    func source(named name: String) -> DataSource? {
        sources.first { $0.name.caseInsensitiveCompare(name) == .orderedSame }
    }

    func hasPassword(_ s: DataSource) -> Bool {
        ((try? secrets().password(for: s.id)) ?? nil) != nil
    }

    /// Add or update. `password`: nil keeps the stored one, "" removes it.
    func save(_ source: DataSource, password: String?) throws {
        var s = source
        s.name = s.name.trimmingCharacters(in: .whitespaces)
        if let problem = s.problem(among: sources) { throw SQLDriverError(problem) }
        guard let file else { throw SQLDriverError("No place to keep data sources on this device") }
        var all = sources.filter { $0.id != s.id }
        all.append(s)
        all.sort { $0.name.localizedStandardCompare($1.name) == .orderedAscending }
        if let password, s.kind.hasPassword {
            try secrets().setPassword(password, for: s.id)
        } else if !s.kind.hasPassword {
            try? secrets().setPassword(nil, for: s.id)
        }
        try file.save(all)
        sources = all
    }

    func delete(_ source: DataSource) throws {
        guard let file else { return }
        let all = sources.filter { $0.id != source.id }
        try file.save(all)
        sources = all
        try? secrets().setPassword(nil, for: source.id)
    }

    /// The driver for a source; `password` overrides the stored one (the
    /// editor's Test connection, before saving).
    func driver(for s: DataSource, password: String? = nil) throws -> any SQLDriver {
        let pw = try password ?? secrets().password(for: s.id)
        switch s.kind {
        case .sqlite:
            return SQLiteDriver(s)
        case .clickhouse:
            return try ClickHouseDriver(s, password: pw)
        case .postgres:
            #if targetEnvironment(macCatalyst)
            return PostgresDriver(s, password: pw)
            #else
            throw SQLDriverError("Postgres runs on the Mac.")
            #endif
        }
    }

    /// Connect and say what answered, or why not (bounded at 15 s).
    func test(_ s: DataSource, password: String?) async -> (ok: Bool, text: String) {
        let driver: any SQLDriver
        do {
            driver = try self.driver(for: s, password: password)
        } catch {
            return (false, SQLRunner.describe(error))
        }
        let task = Task { try await driver.testConnection() }
        let timeout = Task {
            try await Task.sleep(for: .seconds(15))
            task.cancel()
        }
        defer { timeout.cancel() }
        do {
            return (true, try await task.value)
        } catch is CancellationError {
            return (false, "No answer in 15 s")
        } catch {
            return (false, SQLRunner.describe(error))
        }
    }
}
