import Foundation
import TaisceKit

/// Where the app keeps its files.
///
/// The Mac build runs outside the App Sandbox (runnable code blocks need
/// the person's own tools and files), so `~/Library/Application Support`
/// and `~/Library/Caches` are shared with every other app there: the Mac
/// keeps its files in an `ie.null.taisce` folder inside each. The iPhone,
/// and a sandboxed build, use their container's folders as before. A test
/// host (XCTest running inside the app) gets a temp folder, so a test run
/// never touches the real app's data.
enum AppPaths {
    static let bundleID = Bundle.main.bundleIdentifier ?? "ie.null.taisce"

    /// XCTest is running inside this process (hosted unit tests).
    static var isTestHost: Bool {
        let env = ProcessInfo.processInfo.environment
        return env["XCTestConfigurationFilePath"] != nil || env["XCTestBundlePath"] != nil || env["XCTestSessionIdentifier"] != nil
    }

    /// Running inside an App Sandbox container (the iPhone always is).
    static var isSandboxed: Bool {
        #if targetEnvironment(macCatalyst)
        ProcessInfo.processInfo.environment["APP_SANDBOX_CONTAINER_ID"] != nil
        #else
        true
        #endif
    }

    /// The cache databases (one per server) and the migration log.
    static func supportDirectory() throws -> URL {
        let dir: URL
        if isTestHost {
            dir = FileManager.default.temporaryDirectory.appending(path: "taisce-test-host/support", directoryHint: .isDirectory)
        } else {
            let base = try FileManager.default.url(for: .applicationSupportDirectory, in: .userDomainMask, appropriateFor: nil, create: true)
            dir = isSandboxed ? base : base.appending(path: bundleID, directoryHint: .isDirectory)
        }
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        return dir
    }

    /// Rendered diagrams (regenerated on demand).
    static var diagramCache: URL {
        if isTestHost { return FileManager.default.temporaryDirectory.appending(path: "taisce-test-host/diagrams", directoryHint: .isDirectory) }
        let base = URL.cachesDirectory
        return (isSandboxed ? base : base.appending(path: bundleID, directoryHint: .isDirectory)).appending(path: "diagrams", directoryHint: .isDirectory)
    }

    /// First unsandboxed Mac launch: bring the sandboxed build's cache
    /// (with its outbox) and preferences over from the container. Runs
    /// before the model reads a single preference; idempotent; never in a
    /// test host or a sandboxed build. Logged to `migration.log`.
    @discardableResult
    static func migrateSandboxContainer() -> SandboxMigration.Report? {
        #if targetEnvironment(macCatalyst)
        guard !isTestHost, !isSandboxed, let dest = try? supportDirectory() else { return nil }
        let container = URL(fileURLWithPath: Account.current.home)
            .appending(path: "Library/Containers/\(bundleID)/Data", directoryHint: .isDirectory)
        let report = SandboxMigration.run(
            containerData: container, supportDestination: dest, bundleID: bundleID,
            defaults: .standard, domain: bundleID
        )
        SandboxMigration.appendLog(SandboxMigration.logLines(report), to: dest.appending(path: "migration.log"))
        return report
        #else
        return nil
        #endif
    }
}
