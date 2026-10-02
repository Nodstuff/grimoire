import Foundation

/// Moving the Mac app out of the App Sandbox: the sandboxed build kept its
/// data in `~/Library/Containers/<bundle id>/Data/Library/…`; unsandboxed,
/// the same code resolves to `~/Library/…`. On the first unsandboxed launch
/// this copies what matters across:
///
/// - the caches (`Application Support/cache-*.sqlite` with their `-wal` and
///   `-shm`): docs, bodies, to-dos, the sync cursor and the outbox of unsent
///   writes. A cache whose file already exists at the destination is left
///   alone (never overwritten, never mixed with another's WAL);
/// - the preferences (`Preferences/<bundle id>.plist`: server URL, pins, the
///   last workspace, text size, …), merged key by key: a key the new
///   domain already has keeps its value.
///
/// Never deleted or changed: the container. Not copied: `Caches/diagrams`
/// (rendered again on demand) and system state (window restoration,
/// WebKit). Idempotent: every step skips what is already there, and a done
/// marker in the preferences makes later launches return at once. A
/// container the process may not read (macOS's app-data protection: the
/// app was started from a shell or Xcode, not by Launch Services) is
/// logged and left for the next launch, never marked done.
public enum SandboxMigration {
    public static let doneKey = "migration.sandboxContainer.v1"

    public struct Report: Sendable, Hashable {
        /// files copied, relative to Library
        public var copied: [String] = []
        /// caches left because the destination already had one
        public var kept: [String] = []
        public var importedKeys: [String] = []
        /// keys the destination already had (its value kept)
        public var keptKeys: [String] = []
        public var errors: [String] = []
        public var alreadyDone = false
        public var noContainer = false
        public var sourceUnreadable = false

        /// finished: the marker is (or was already) set
        public var complete: Bool { alreadyDone || (errors.isEmpty && !sourceUnreadable) }
    }

    /// - containerData: `~/Library/Containers/<bundle id>/Data`
    /// - supportDestination: where the unsandboxed app keeps its caches
    /// - defaults/domain: the unsandboxed preferences (`.standard` and the
    ///   bundle id in the app; a suite in tests)
    /// - log: lines for `migration.log` (names, never values)
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
        do {
            let names = try listing(support, fileManager)
            try fileManager.createDirectory(at: supportDestination, withIntermediateDirectories: true)
            for main in names.filter({ $0.hasPrefix("cache-") && $0.hasSuffix(".sqlite") }).sorted() {
                let target = supportDestination.appendingPathComponent(main)
                if fileManager.fileExists(atPath: target.path) {
                    report.kept.append("Application Support/\(main)")
                    continue
                }
                // the WAL and SHM first, the database last: an interrupted
                // copy leaves no main file, so the next launch starts over
                for suffix in ["-wal", "-shm"] {
                    let stray = supportDestination.appendingPathComponent(main + suffix)
                    if fileManager.fileExists(atPath: stray.path) { try fileManager.removeItem(at: stray) }
                }
                for name in [main + "-wal", main + "-shm", main] where names.contains(name) {
                    let tmp = supportDestination.appendingPathComponent(".\(name).migrating")
                    try? fileManager.removeItem(at: tmp)
                    try fileManager.copyItem(at: support.appendingPathComponent(name), to: tmp)
                    try fileManager.moveItem(at: tmp, to: supportDestination.appendingPathComponent(name))
                    report.copied.append("Application Support/\(name)")
                }
            }
        } catch let e as CocoaError where e.code == .fileReadNoSuchFile {
            // no Application Support in the container: nothing cached
        } catch let e as CocoaError where e.code == .fileReadNoPermission {
            report.sourceUnreadable = true
            report.errors.append("Application Support: not readable (\(e.code.rawValue))")
            return report
        } catch {
            report.errors.append("Application Support: \(error.localizedDescription)")
        }

        // preferences
        let plist = library.appendingPathComponent("Preferences/\(bundleID).plist")
        do {
            let data = try Data(contentsOf: plist)
            guard let src = try PropertyListSerialization.propertyList(from: data, format: nil) as? [String: Any] else {
                throw CocoaError(.propertyListReadCorrupt)
            }
            for key in src.keys.sorted() where key != doneKey {
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

        if report.errors.isEmpty {
            dest[doneKey] = ISO8601DateFormatter().string(from: now)
        }
        if !report.importedKeys.isEmpty || report.errors.isEmpty {
            defaults.setPersistentDomain(dest, forName: domain)
        }
        return report
    }

    static func listing(_ dir: URL, _ fm: FileManager) throws -> Set<String> {
        Set(try fm.contentsOfDirectory(atPath: dir.path))
    }

    /// The report as log lines: what moved, what stayed, what failed.
    public static func logLines(_ r: Report, now: Date = .now) -> [String] {
        let t = ISO8601DateFormatter().string(from: now)
        if r.alreadyDone { return [] }
        if r.noContainer { return ["\(t) no sandbox container: nothing to migrate"] }
        var lines = ["\(t) sandbox container migration\(r.complete ? "" : " (incomplete, will retry next launch)")"]
        lines += r.copied.map { "  copied \($0)" }
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
