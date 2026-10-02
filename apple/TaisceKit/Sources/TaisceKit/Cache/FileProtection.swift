import Foundation

extension Cache {
    /// The cache holds doc content: readable after the first unlock (so
    /// sync and notification actions work while locked), never before.
    public static let fileProtection = FileProtectionType.completeUntilFirstUserAuthentication

    /// Data protection classes are an iOS device feature: macOS (and Mac
    /// Catalyst, unsandboxed) keeps the cache under FileVault.
    public static var appliesFileProtection: Bool {
        #if os(iOS) && !targetEnvironment(macCatalyst)
        true
        #else
        false
        #endif
    }

    /// The database's directory, so files SQLite creates later (journals,
    /// a recreated WAL) inherit the class.
    static func protectDirectory(ofDatabaseAt path: String) throws {
        guard appliesFileProtection else { return }
        let dir = URL(filePath: path).deletingLastPathComponent().path(percentEncoded: false)
        try FileManager.default.setAttributes([.protectionKey: fileProtection], ofItemAtPath: dir)
    }

    /// The database and its WAL and SHM files (DatabasePool opens in WAL
    /// mode, so both exist once it is open).
    static func protectFiles(ofDatabaseAt path: String) throws {
        guard appliesFileProtection else { return }
        for file in [path, path + "-wal", path + "-shm"] where FileManager.default.fileExists(atPath: file) {
            try FileManager.default.setAttributes([.protectionKey: fileProtection], ofItemAtPath: file)
        }
    }
}
