import Foundation

extension Cache {
    /// The cache holds doc content: readable after the first unlock (so
    /// sync and notification actions work while locked), never before.
    public static let fileProtection = FileProtectionType.completeUntilFirstUserAuthentication

    /// The database's directory, so files SQLite creates later (journals,
    /// a recreated WAL) inherit the class.
    static func protectDirectory(ofDatabaseAt path: String) throws {
        #if os(iOS)
        let dir = URL(filePath: path).deletingLastPathComponent().path(percentEncoded: false)
        try FileManager.default.setAttributes([.protectionKey: fileProtection], ofItemAtPath: dir)
        #endif
    }

    /// The database and its WAL and SHM files (DatabasePool opens in WAL
    /// mode, so both exist once it is open).
    static func protectFiles(ofDatabaseAt path: String) throws {
        #if os(iOS)
        for file in [path, path + "-wal", path + "-shm"] where FileManager.default.fileExists(atPath: file) {
            try FileManager.default.setAttributes([.protectionKey: fileProtection], ofItemAtPath: file)
        }
        #endif
    }
}
