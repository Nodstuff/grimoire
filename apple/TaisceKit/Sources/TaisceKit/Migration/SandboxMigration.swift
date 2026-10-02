import CryptoKit
import Foundation
import GRDB

/// Moving the Mac app out of the App Sandbox: the sandboxed build kept its
/// data in `~/Library/Containers/<bundle id>/Data/Library/…`; unsandboxed,
/// the same code resolves to `~/Library/…`. On the first unsandboxed launch
/// this copies what matters across:
///
/// - the caches (`Application Support/cache-*.sqlite` with their `-wal` and
///   `-shm`): docs, bodies, to-dos, the sync cursor and the outbox of unsent
///   writes. Each is staged beside the destination, read there, and moved in
///   database-last (a failed move takes back what it moved: never a WAL
///   beside the wrong database). A cache already at the destination is:
///   kept if this migration put it there (`createdKey`); set aside under
///   `.replaced-<time>/` and replaced if it has no owner and no queued writes
///   (the app made it before it could migrate); kept if the container's copy
///   has nothing unsent; otherwise an error (both kept, never marked done);
/// - the preferences (`Preferences/<bundle id>.plist`: server URL, pins, the
///   last workspace, text size, …), merged key by key: a key the new
///   domain already has keeps its value.
///
/// Never deleted or changed: the container. Not copied: `Caches/diagrams`
/// (rendered again on demand) and system state (window restoration,
/// WebKit). Idempotent, and a done marker makes later launches return at
/// once. A container the process may not read (macOS's app-data
/// protection: the app was started from a shell or Xcode, not by Launch
/// Services) is logged and left for the next launch, never marked done; the
/// app must not open a cache while a pass is incomplete (`Report.complete`).
public enum SandboxMigration {
    public static let doneKey = "migration.sandboxContainer.v1"
    /// the destination caches this migration copied, so a later pass knows
    /// which existing files are its own
    public static let createdKey = "migration.sandboxContainer.created"

    public struct Conflict: Sendable, Hashable {
        /// the cache file, e.g. `cache-taisce.null.ie-0.sqlite`
        public var name: String
        /// unsent changes in the container's copy (nil: couldn't read it)
        public var unsent: Int?

        public init(name: String, unsent: Int?) {
            self.name = name
            self.unsent = unsent
        }
    }

    public struct Report: Sendable, Hashable {
        /// files copied, relative to Library
        public var copied: [String] = []
        /// caches left because the destination already had one
        public var kept: [String] = []
        /// empty caches the app made before it could migrate, set aside
        /// under `.replaced-<time>/` and replaced by the container's
        public var replaced: [String] = []
        public var importedKeys: [String] = []
        /// keys the destination already had (its value kept)
        public var keptKeys: [String] = []
        public var errors: [String] = []
        /// caches that are both here (with data) and in the container (with
        /// unsent changes, or unreadable): a person decides
        /// (`keepDestination` / `useContainerCopy`)
        public var conflicts: [Conflict] = []
        public var alreadyDone = false
        public var noContainer = false
        public var sourceUnreadable = false

        public init() {}

        /// finished: the marker is (or was already) set
        public var complete: Bool { alreadyDone || (errors.isEmpty && !sourceUnreadable) }
    }

    /// - containerData: `~/Library/Containers/<bundle id>/Data`
    /// - supportDestination: where the unsandboxed app keeps its caches
    /// - defaults/domain: the unsandboxed preferences (`.standard` and the
    ///   bundle id in the app; a suite in tests)
    public static func run(
        containerData: URL, supportDestination: URL, bundleID: String,
        defaults: UserDefaults, domain: String,
        fileManager: FileManager = .default, now: Date = .now
    ) -> Report {
        var report = Report()
        var dest = defaults.persistentDomain(forName: domain) ?? [:]
        if dest[doneKey] != nil {
            report.alreadyDone = true
            return report
        }
        let library = containerData.appendingPathComponent("Library", isDirectory: true)
        // "no container" only on a definite ENOENT: app-data protection
        // can make an existing one look unreadable, never absent
        do {
            _ = try listing(containerData, fileManager)
        } catch let e as CocoaError where e.code == .fileReadNoSuchFile {
            report.noContainer = true
            dest[doneKey] = ISO8601DateFormatter().string(from: now)
            defaults.setPersistentDomain(dest, forName: domain)
            return report
        } catch {
            report.sourceUnreadable = true
            report.errors.append("container: not readable (\((error as? CocoaError)?.code.rawValue ?? -1))")
            return report
        }

        // caches
        let support = library.appendingPathComponent("Application Support", isDirectory: true)
        var created = Set(dest[createdKey] as? [String] ?? [])
        var names: Set<String> = []
        do {
            names = try listing(support, fileManager)
        } catch let e as CocoaError where e.code == .fileReadNoSuchFile {
            // no Application Support in the container: nothing cached
        } catch let e as CocoaError where e.code == .fileReadNoPermission {
            report.sourceUnreadable = true
            report.errors.append("Application Support: not readable (\(e.code.rawValue))")
            return report
        } catch {
            report.errors.append("Application Support: \(error.localizedDescription)")
        }
        let mains = names.filter { $0.hasPrefix("cache-") && $0.hasSuffix(".sqlite") }.sorted()
        if !mains.isEmpty {
            let staging = supportDestination.appendingPathComponent(".migration-staging", isDirectory: true)
            for main in mains {
                let label = "Application Support/\(main)"
                do {
                    try fileManager.createDirectory(at: supportDestination, withIntermediateDirectories: true)
                    try? fileManager.removeItem(at: staging)
                    try fileManager.createDirectory(at: staging, withIntermediateDirectories: true)
                    defer { try? fileManager.removeItem(at: staging) }
                    let group = [main + "-wal", main + "-shm", main].filter { names.contains($0) }
                    // a copy of the container's cache, read here, never in place
                    for name in group {
                        try fileManager.copyItem(at: support.appendingPathComponent(name), to: staging.appendingPathComponent(name))
                    }
                    let incoming = inspect(staging.appendingPathComponent(main).path)
                    let target = supportDestination.appendingPathComponent(main)
                    if fileManager.fileExists(atPath: target.path) {
                        // ours: recorded, or the very bytes staged (a crash
                        // between moving it in and recording it; or a build
                        // that predates the record)
                        let identical = sameBytes(target, staging.appendingPathComponent(main))
                        if created.contains(main) || identical {
                            report.kept.append(label)
                            if !created.contains(main) {
                                created.insert(main)
                                dest[createdKey] = created.sorted()
                                defaults.setPersistentDomain(dest, forName: domain)
                            }
                            continue
                        }
                        if let e = inspect(target.path), e.owner == nil, e.outbox == 0 {
                            // made by the app in a launch that couldn't migrate
                            let aside = supportDestination.appendingPathComponent(".replaced-\(Int(now.timeIntervalSince1970))", isDirectory: true)
                            try fileManager.createDirectory(at: aside, withIntermediateDirectories: true)
                            for name in [main, main + "-wal", main + "-shm"] {
                                let u = supportDestination.appendingPathComponent(name)
                                if fileManager.fileExists(atPath: u.path) { try fileManager.moveItem(at: u, to: aside.appendingPathComponent(name)) }
                            }
                            report.replaced.append(label)
                        } else if incoming?.outbox == 0 {
                            // nothing unsent in the container's copy: this one stays
                            report.kept.append(label)
                            continue
                        } else {
                            let n = incoming.map { "\($0.outbox)" } ?? "an unknown number of"
                            report.errors.append("\(label): one with data is already here and the old one has \(n) unsent changes; both kept, nothing copied")
                            report.conflicts.append(Conflict(name: main, unsent: incoming?.outbox))
                            continue
                        }
                    }
                    try install(group, from: staging, to: supportDestination, fileManager)
                    report.copied += group.map { "Application Support/\($0)" }
                    created.insert(main)
                    // at once: a later failure in this pass must not forget it
                    dest[createdKey] = created.sorted()
                    defaults.setPersistentDomain(dest, forName: domain)
                } catch let e as CocoaError where e.code == .fileReadNoPermission {
                    report.sourceUnreadable = true
                    report.errors.append("\(label): not readable (\(e.code.rawValue))")
                } catch {
                    report.errors.append("\(label): \(error.localizedDescription)")
                }
            }
        }

        // preferences
        let plist = library.appendingPathComponent("Preferences/\(bundleID).plist")
        do {
            let data = try Data(contentsOf: plist)
            guard let src = try PropertyListSerialization.propertyList(from: data, format: nil) as? [String: Any] else {
                throw CocoaError(.propertyListReadCorrupt)
            }
            for key in src.keys.sorted() where key != doneKey && key != createdKey {
                if dest[key] != nil {
                    report.keptKeys.append(key)
                } else {
                    dest[key] = src[key]
                    report.importedKeys.append(key)
                }
            }
        } catch let e as CocoaError where e.code == .fileReadNoSuchFile {
            // never wrote a preference
        } catch let e as CocoaError where e.code == .fileReadNoPermission {
            report.sourceUnreadable = true
            report.errors.append("Preferences: not readable (\(e.code.rawValue))")
        } catch {
            report.errors.append("Preferences: \(error.localizedDescription)")
        }

        if report.errors.isEmpty && !report.sourceUnreadable {
            dest[doneKey] = ISO8601DateFormatter().string(from: now)
        }
        defaults.setPersistentDomain(dest, forName: domain)
        return report
    }

    /// Same size and SHA-256.
    static func sameBytes(_ a: URL, _ b: URL) -> Bool {
        guard let sa = try? FileManager.default.attributesOfItem(atPath: a.path)[.size] as? Int,
              let sb = try? FileManager.default.attributesOfItem(atPath: b.path)[.size] as? Int, sa == sb,
              let da = try? Data(contentsOf: a, options: .mappedIfSafe),
              let db = try? Data(contentsOf: b, options: .mappedIfSafe)
        else { return false }
        return SHA256.hash(data: da) == SHA256.hash(data: db)
    }

    /// A conflict, resolved by keeping the cache already here: the
    /// container's copy (and its unsent changes) stays where it is,
    /// untouched; the next pass counts this one as the migration's own.
    /// Returns the log lines.
    @discardableResult
    public static func keepDestination(_ c: Conflict, defaults: UserDefaults, domain: String, now: Date = .now) -> [String] {
        var dest = defaults.persistentDomain(forName: domain) ?? [:]
        var created = Set(dest[createdKey] as? [String] ?? [])
        created.insert(c.name)
        dest[createdKey] = created.sorted()
        defaults.setPersistentDomain(dest, forName: domain)
        let n = c.unsent.map(String.init) ?? "an unknown number of"
        return ["\(ISO8601DateFormatter().string(from: now)) kept the cache already here for \(c.name); the old one, with \(n) unsent changes, stays in the container untouched"]
    }

    /// A conflict, resolved by using the container's copy: the cache here
    /// is set aside (never deleted) under `.replaced-<time>/`, so the next
    /// pass copies the container's in. Returns the log lines.
    @discardableResult
    public static func useContainerCopy(_ c: Conflict, supportDestination: URL, fileManager: FileManager = .default, now: Date = .now) throws -> [String] {
        let aside = supportDestination.appendingPathComponent(".replaced-\(Int(now.timeIntervalSince1970))", isDirectory: true)
        try fileManager.createDirectory(at: aside, withIntermediateDirectories: true)
        for name in [c.name, c.name + "-wal", c.name + "-shm"] {
            let u = supportDestination.appendingPathComponent(name)
            if fileManager.fileExists(atPath: u.path) { try fileManager.moveItem(at: u, to: aside.appendingPathComponent(name)) }
        }
        return ["\(ISO8601DateFormatter().string(from: now)) set aside the cache here for \(c.name) (in \(aside.lastPathComponent)) to use the old one"]
    }

    /// Move a staged cache into place, the database last. If any move
    /// fails, the pieces already moved are removed again (they are copies):
    /// never a WAL or SHM beside the wrong database.
    static func install(_ group: [String], from staging: URL, to dest: URL, _ fm: FileManager) throws {
        guard let main = group.first(where: { $0.hasSuffix(".sqlite") }) else { return }
        for suffix in ["-wal", "-shm"] {
            let stray = dest.appendingPathComponent(main + suffix)
            if fm.fileExists(atPath: stray.path) { try fm.removeItem(at: stray) }
        }
        var moved: [URL] = []
        do {
            for name in group {
                let to = dest.appendingPathComponent(name)
                try fm.moveItem(at: staging.appendingPathComponent(name), to: to)
                moved.append(to)
            }
        } catch {
            for u in moved { try? fm.removeItem(at: u) }
            throw error
        }
    }

    /// Owner and queued writes of a cache file; nil if it can't be read as one.
    static func inspect(_ path: String) -> (owner: String?, outbox: Int)? {
        guard let q = try? DatabaseQueue(path: path) else { return nil }
        return try? q.read { db -> (String?, Int) in
            // throws for a file that isn't a database
            _ = try Int.fetchOne(db, sql: "SELECT count(*) FROM sqlite_master")
            let outbox = try db.tableExists("outbox") ? (Int.fetchOne(db, sql: "SELECT count(*) FROM outbox") ?? 0) : 0
            let owner = try db.tableExists("cache_meta") ? String.fetchOne(db, sql: "SELECT value FROM cache_meta WHERE key = 'owner'") : nil
            return (owner, outbox)
        }
    }

    static func listing(_ dir: URL, _ fm: FileManager) throws -> Set<String> {
        Set(try fm.contentsOfDirectory(atPath: dir.path))
    }

    /// Why the app can't open its data yet, for the "finish moving" screen;
    /// nil when it may.
    public static func blockingReason(_ r: Report?) -> String? {
        guard let r, !r.complete else { return nil }
        if r.sourceUnreadable {
            return "Quit and open Taisce from Finder to finish moving your data."
        }
        if !r.conflicts.isEmpty {
            return "Your data is in two places: here, and in the storage the sandboxed version used. Choose which copy to use."
        }
        return "Taisce couldn't finish moving your data from the sandboxed version (see migration.log in ~/Library/Application Support/ie.null.taisce). Nothing has been lost; quit and open it again from Finder."
    }

    /// The report as log lines: what moved, what stayed, what failed.
    public static func logLines(_ r: Report, now: Date = .now) -> [String] {
        let t = ISO8601DateFormatter().string(from: now)
        if r.alreadyDone { return [] }
        if r.noContainer { return ["\(t) no sandbox container: nothing to migrate"] }
        var lines = ["\(t) sandbox container migration\(r.complete ? "" : " (incomplete, will retry next launch)")"]
        lines += r.copied.map { "  copied \($0)" }
        lines += r.replaced.map { "  replaced an empty \($0) (set aside in .replaced-…)" }
        lines += r.kept.map { "  kept existing \($0)" }
        if !r.importedKeys.isEmpty { lines.append("  imported preferences: \(r.importedKeys.joined(separator: ", "))") }
        if !r.keptKeys.isEmpty { lines.append("  kept existing preferences: \(r.keptKeys.joined(separator: ", "))") }
        lines += r.errors.map { "  error: \($0)" }
        return lines
    }

    public static func appendLog(_ lines: [String], to file: URL) {
        guard !lines.isEmpty else { return }
        let text = lines.joined(separator: "\n") + "\n"
        if let h = try? FileHandle(forWritingTo: file) {
            defer { try? h.close() }
            _ = try? h.seekToEnd()
            try? h.write(contentsOf: Data(text.utf8))
        } else {
            try? Data(text.utf8).write(to: file)
        }
    }
}
