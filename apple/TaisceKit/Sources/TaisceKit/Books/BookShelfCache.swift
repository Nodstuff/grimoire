import Foundation

/// The last shelf this device saw, kept as one JSON file so Books opens at
/// once (and offline) on what was there; the server's answer replaces it.
/// Per person: sign-out removes it (`remove()`), like the doc cache.
public struct BookShelfCache: Sendable {
    public let file: URL

    public init(file: URL) {
        self.file = file
    }

    struct Stored: Codable {
        struct Reading: Codable {
            var book: Book
            var progress: BookProgress
        }

        var version = 1
        var books: [Book]
        var progress: [String: BookProgress]
        var reading: [Reading]
        var ereaderCount: Int
        var savedAt: Date
    }

    /// The stored shelf and when it was saved, or nil (none, or unreadable).
    public func load() -> (shelf: BookShelf, savedAt: Date)? {
        guard let data = try? Data(contentsOf: file),
              let s = try? JSONDecoder().decode(Stored.self, from: data), s.version == 1
        else { return nil }
        let shelf = BookShelf(books: s.books, progress: s.progress,
                              reading: s.reading.map { ReadingBook(book: $0.book, progress: $0.progress) },
                              ereaderCount: s.ereaderCount)
        return (shelf, s.savedAt)
    }

    public func save(_ shelf: BookShelf, at: Date = .now) throws {
        let s = Stored(books: shelf.books, progress: shelf.progress,
                       reading: shelf.reading.map { .init(book: $0.book, progress: $0.progress) },
                       ereaderCount: shelf.ereaderCount, savedAt: at)
        try FileManager.default.createDirectory(at: file.deletingLastPathComponent(), withIntermediateDirectories: true)
        try JSONEncoder().encode(s).write(to: file, options: [.atomic, .completeFileProtectionUntilFirstUserAuthentication])
    }

    public func remove() {
        try? FileManager.default.removeItem(at: file)
    }
}
