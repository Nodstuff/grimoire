import Foundation
import Observation
import TaisceKit
import UIKit

/// The person's books on this device: the shelf (cached as one JSON file so
/// Books opens at once and offline), their covers (memory + disk), and the
/// calls the Books screens make. One per app (`AppModel.books`); sign-out
/// forgets everything (`reset`).
@MainActor @Observable
final class BookStore {
    enum Availability: Equatable {
        /// not asked yet (or no connection yet)
        case unknown
        case available
        /// the server keeps no library for us: LOCAL mode, or no bucket (404)
        case unavailable
    }

    @ObservationIgnored weak var app: AppModel?
    private(set) var shelf = BookShelf()
    private(set) var availability: Availability = .unknown
    /// a load is running
    private(set) var isLoading = false
    /// the shelf showing is the cached one (a refresh hasn't answered yet, or failed)
    private(set) var isStale = false
    /// the last load's failure, when there is no shelf to fall back on (or a refresh failed)
    var loadError: String?
    private(set) var savedAt: Date?
    @ObservationIgnored private var loadTask: Task<Void, Never>?
    @ObservationIgnored private var cacheLoaded = false
    @ObservationIgnored let covers = CoverCache()

    var api: APIClient? { app?.api }

    /// Per server: the shelf and covers of one server never show for another.
    var serverKey: String {
        let s = app?.serverURL ?? "none"
        var h: UInt64 = 0xcbf2_9ce4_8422_2325
        for b in s.utf8 { h = (h ^ UInt64(b)) &* 0x100_0000_01b3 }
        return String(h, radix: 16)
    }

    var cacheFile: URL {
        Self.directory.appending(path: "\(serverKey)/shelf.json")
    }

    static var directory: URL {
        if AppPaths.isTestHost { return FileManager.default.temporaryDirectory.appending(path: "taisce-test-host/books", directoryHint: .isDirectory) }
        let base = URL.cachesDirectory
        return (AppPaths.isSandboxed ? base : base.appending(path: AppPaths.bundleID, directoryHint: .isDirectory)).appending(path: "books", directoryHint: .isDirectory)
    }

    func book(_ id: String) -> Book? { shelf.books.first { $0.id == id } }
    func progress(_ id: String) -> BookProgress? { shelf.progress[id] }

    /// The cached shelf (once), then the server's.
    func load(force: Bool = false) async {
        if !cacheLoaded {
            cacheLoaded = true
            covers.directory = Self.directory.appending(path: "\(serverKey)/covers", directoryHint: .isDirectory)
            if shelf.books.isEmpty, let cached = BookShelfCache(file: cacheFile).load() {
                shelf = cached.shelf
                savedAt = cached.savedAt
                isStale = true
                availability = .available
            }
        }
        if let loadTask, !force {
            await loadTask.value
            return
        }
        let task = Task { await fetch() }
        loadTask = task
        await task.value
        loadTask = nil
    }

    private func fetch() async {
        guard let api else { return }
        isLoading = true
        defer { isLoading = false }
        do {
            let books = try await api.allBooks()
            async let reading = try? api.currentlyReading()
            async let ereaders = try? api.ereaders()
            let progress = (try? await api.bookProgress(ids: books.map(\.id))) ?? shelf.progress
            let next = BookShelf(books: books, progress: progress, reading: await reading ?? [],
                                 ereaderCount: (await ereaders)?.filter(\.isActive).count ?? shelf.ereaderCount)
            shelf = next
            availability = .available
            isStale = false
            loadError = nil
            savedAt = .now
            try? BookShelfCache(file: cacheFile).save(next)
        } catch APIError.notFound, APIError.notAPIRoute {
            availability = .unavailable
            isStale = false
            shelf = BookShelf()
            BookShelfCache(file: cacheFile).remove()
        } catch {
            if availability == .unknown { availability = .available }
            isStale = !shelf.books.isEmpty
            loadError = Self.describe(error)
        }
    }

    static func describe(_ error: any Error) -> String {
        if let e = error as? APIError {
            switch e {
            case let .server(m): return m
            case .unauthorized: return "Sign in again to see your books."
            case let .http(status) where status >= 500: return "The server isn't answering right now."
            default: break
            }
        }
        if (error as? URLError) != nil { return "You're offline." }
        return error.localizedDescription
    }

    // MARK: edits

    func replace(_ book: Book) {
        guard let i = shelf.books.firstIndex(where: { $0.id == book.id }) else { return }
        shelf.books[i] = book
        for j in shelf.reading.indices where shelf.reading[j].book.id == book.id { shelf.reading[j].book = book }
        try? BookShelfCache(file: cacheFile).save(shelf)
    }

    func update(_ id: String, patch: BookPatch) async throws -> Book {
        guard let api else { throw APIError.unauthorized }
        let b = try await api.updateBook(id, patch: patch)
        replace(b)
        return b
    }

    func delete(_ id: String) async throws {
        guard let api else { throw APIError.unauthorized }
        try await api.deleteBook(id)
        shelf.books.removeAll { $0.id == id }
        shelf.reading.removeAll { $0.book.id == id }
        shelf.progress[id] = nil
        try? BookShelfCache(file: cacheFile).save(shelf)
    }

    func chooseCover(_ id: String, candidate: String?) async throws -> Book {
        guard let api else { throw APIError.unauthorized }
        let b = try await api.chooseCover(id, candidate: candidate, unlessChosen: false)
        replace(b)
        return b
    }

    /// The reader's place moved: the shelf's progress follows (percent 0–100).
    func noteProgress(_ id: String, fraction: Double, device: String) {
        guard book(id) != nil else { return }
        let pct = min(100, max(0, fraction * 100))
        let status: ProgressStatus = fraction >= 0.98 ? .finished : fraction > 0 ? .reading : .unread
        let p = BookProgress(percentage: pct, device: device, updated: Date.now.formatted(.iso8601), status: status)
        shelf.progress[id] = p
        if let i = shelf.reading.firstIndex(where: { $0.book.id == id }) {
            shelf.reading[i].progress = p
            if status != .reading { shelf.reading.remove(at: i) }
        } else if status == .reading, let b = book(id) {
            shelf.reading.insert(ReadingBook(book: b, progress: p), at: 0)
        }
    }

    /// Add EPUBs picked from Files: one request each 20 MB (a bigger file alone).
    func upload(_ files: [URL]) async throws -> [BookUploadResult] {
        guard let api else { throw APIError.unauthorized }
        var batches: [[(name: String, data: Data)]] = [[]]
        var bytes = 0
        for url in files {
            let scoped = url.startAccessingSecurityScopedResource()
            defer { if scoped { url.stopAccessingSecurityScopedResource() } }
            let data = try Data(contentsOf: url)
            if bytes + data.count > 20 * 1024 * 1024, !(batches.last?.isEmpty ?? true) {
                batches.append([])
                bytes = 0
            }
            batches[batches.count - 1].append((name: url.lastPathComponent, data: data))
            bytes += data.count
        }
        var results: [BookUploadResult] = []
        for batch in batches where !batch.isEmpty {
            results += try await api.uploadBooks(batch)
        }
        await load(force: true)
        return results
    }

    /// The EPUB written to a temporary file named as the feed names it, to
    /// share (Open in Books, AirDrop, Save to Files).
    func exportFile(_ source: ReadingSource, name: String) async throws -> URL {
        guard let api else { throw APIError.unauthorized }
        let data = try await api.epub(source)
        let dir = FileManager.default.temporaryDirectory.appending(path: "epub-share/\(UUID().uuidString)", directoryHint: .isDirectory)
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        let file = dir.appending(path: name)
        try data.write(to: file, options: .atomic)
        return file
    }

    // MARK: covers

    var coverLoader: CoverImageLoader {
        let covers = self.covers
        let api = self.api
        return CoverImageLoader(
            load: { url in
                guard let api else { return nil }
                return await covers.image(url) { try await api.bookImage(at: url) }
            },
            cached: { covers.memory($0) }
        )
    }

    func coverURL(_ book: Book, embedded: Bool = false) -> URL? { api?.coverURL(book, embedded: embedded) }

    // MARK: sign-out

    /// Another server: its own shelf and covers (each server's stay on disk).
    func serverChanged() {
        loadTask?.cancel()
        loadTask = nil
        shelf = BookShelf()
        availability = .unknown
        isStale = false
        loadError = nil
        savedAt = nil
        cacheLoaded = false
        covers.clear()
    }

    /// Forget this person's books on this device: the shelf, its cache file,
    /// the covers, the reader's local places.
    func reset() {
        loadTask?.cancel()
        loadTask = nil
        shelf = BookShelf()
        availability = .unknown
        isStale = false
        loadError = nil
        savedAt = nil
        cacheLoaded = false
        covers.clear()
        try? FileManager.default.removeItem(at: Self.directory)
        ReaderPlaces.forgetAll()
    }
}

/// Cover images: decoded ones in memory, the bytes on disk (Caches), keyed
/// by URL (which carries the cover's version, so a new cover is a new key).
final class CoverCache: @unchecked Sendable {
    private let memoryCache = NSCache<NSURL, UIImage>()
    private let lock = NSLock()
    private var _directory: URL?
    /// failures this run (no retry storm for a cover that isn't there)
    private var missing: Set<URL> = []

    init() {
        memoryCache.countLimit = 300
    }

    var directory: URL? {
        get { lock.withLock { _directory } }
        set { lock.withLock { _directory = newValue } }
    }

    func memory(_ url: URL) -> UIImage? { memoryCache.object(forKey: url as NSURL) }

    private func file(_ url: URL) -> URL? {
        guard let dir = directory else { return nil }
        var h: UInt64 = 0xcbf2_9ce4_8422_2325
        for b in url.absoluteString.utf8 { h = (h ^ UInt64(b)) &* 0x100_0000_01b3 }
        return dir.appending(path: String(h, radix: 16) + ".img")
    }

    func image(_ url: URL, fetch: @Sendable () async throws -> Data) async -> UIImage? {
        if let m = memory(url) { return m }
        if lock.withLock({ missing.contains(url) }) { return nil }
        if let f = file(url), let data = try? Data(contentsOf: f), let img = await Self.decode(data) {
            memoryCache.setObject(img, forKey: url as NSURL)
            return img
        }
        do {
            let data = try await fetch()
            guard let img = await Self.decode(data) else { throw URLError(.cannotDecodeContentData) }
            memoryCache.setObject(img, forKey: url as NSURL)
            if let f = file(url) {
                try? FileManager.default.createDirectory(at: f.deletingLastPathComponent(), withIntermediateDirectories: true)
                try? data.write(to: f, options: .atomic)
            }
            return img
        } catch APIError.notFound {
            lock.withLock { _ = missing.insert(url) }
            return nil
        } catch {
            return nil
        }
    }

    /// Decoded off the main thread, at most 600 px tall (a cover is never shown bigger).
    static func decode(_ data: Data) async -> UIImage? {
        await Task.detached(priority: .utility) {
            guard let img = UIImage(data: data) else { return nil }
            let maxH: CGFloat = 900
            guard img.size.height > maxH else { return img.preparingForDisplay() ?? img }
            let size = CGSize(width: img.size.width * maxH / img.size.height, height: maxH)
            return img.preparingThumbnail(of: size) ?? img
        }.value
    }

    func clear() {
        memoryCache.removeAllObjects()
        lock.withLock { missing.removeAll() }
    }
}

/// Where this device last was in each book (`LocalPlace`), and its reader
/// device id. Per person: sign-out forgets the places (`forgetAll`); the
/// device id is the install's.
enum ReaderPlaces {
    static let placesKey = "reader.places"
    static let deviceKey = "reader.deviceID"

    static var deviceID: String {
        let d = UserDefaults.standard
        if let id = d.string(forKey: deviceKey) { return id }
        let id = UUID().uuidString.lowercased()
        d.set(id, forKey: deviceKey)
        return id
    }

    static func load(_ source: ReadingSource, server: String) -> LocalPlace? {
        guard let all = UserDefaults.standard.dictionary(forKey: placesKey) as? [String: Data],
              let data = all["\(server)/\(source.key)"]
        else { return nil }
        return try? JSONDecoder().decode(LocalPlace.self, from: data)
    }

    static func save(_ place: LocalPlace, _ source: ReadingSource, server: String) {
        var all = (UserDefaults.standard.dictionary(forKey: placesKey) as? [String: Data]) ?? [:]
        all["\(server)/\(source.key)"] = try? JSONEncoder().encode(place)
        UserDefaults.standard.set(all, forKey: placesKey)
    }

    static func forgetAll() {
        UserDefaults.standard.removeObject(forKey: placesKey)
    }
}
