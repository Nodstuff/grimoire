import Foundation

/// The EPUB library (`library.rs`, `kosync.rs`, `covers.rs`, `reader.rs`):
/// a person's own surface, SERVER mode with a bucket (LOCAL answers 404,
/// which `BookStore` shows as "Books live on your Taisce server").
public protocol BookService: Sendable {
    func books(search: String?, before: String?, limit: Int?) async throws -> BookPage
    func bookProgress(ids: [String]) async throws -> [String: BookProgress]
    func currentlyReading() async throws -> [ReadingBook]
    func ereaders() async throws -> [DevicePasswordReader]
    func updateBook(_ id: String, patch: BookPatch) async throws -> Book
    func deleteBook(_ id: String) async throws
    func epub(_ source: ReadingSource) async throws -> Data
    func position(_ source: ReadingSource) async throws -> PositionRead
    func putPosition(_ source: ReadingSource, _ body: PositionWrite) async -> PositionWriteResult
    func coverImage(_ id: String, version: Int64?, embedded: Bool) async throws -> Data
    func coverCandidates(_ id: String) async throws -> CoverCandidates
    func findCoverCandidates(_ id: String, refresh: Bool, title: String?, author: String?) async throws -> CoverCandidates
    func candidateImage(_ path: String) async throws -> Data
    func chooseCover(_ id: String, candidate: String?, unlessChosen: Bool) async throws -> Book
    func uploadBooks(_ files: [(name: String, data: Data)]) async throws -> [BookUploadResult]
}

extension APIClient: BookService {
    /// One `/api/books/progress` request carries at most this many ids.
    public static let progressChunk = 200

    public func books(search: String? = nil, before: String? = nil, limit: Int? = nil) async throws -> BookPage {
        var q: [URLQueryItem] = []
        if let s = search?.trimmingCharacters(in: .whitespacesAndNewlines), !s.isEmpty { q.append(URLQueryItem(name: "search", value: s)) }
        if let before, !before.isEmpty { q.append(URLQueryItem(name: "before", value: before)) }
        if let limit { q.append(URLQueryItem(name: "limit", value: String(limit))) }
        return try await get("/api/books", query: q)
    }

    /// Every book, newest first (pages of 200 until `next_before` runs out).
    public func allBooks() async throws -> [Book] {
        var out: [Book] = []
        var before: String?
        repeat {
            let page = try await books(search: nil, before: before, limit: 200)
            out += page.books
            before = page.nextBefore
        } while before != nil
        return out
    }

    /// Progress for a set of books; ids without a row are absent.
    public func bookProgress(ids: [String]) async throws -> [String: BookProgress] {
        struct Answer: Decodable { var progress: [String: BookProgress?] }
        var out: [String: BookProgress] = [:]
        var i = 0
        while i < ids.count {
            let part = ids[i..<min(ids.count, i + Self.progressChunk)]
            i += Self.progressChunk
            let a: Answer = try await get("/api/books/progress", query: [URLQueryItem(name: "ids", value: part.joined(separator: ","))])
            for (k, v) in a.progress { if let v { out[k] = v } }
        }
        return out
    }

    public func currentlyReading() async throws -> [ReadingBook] {
        struct Answer: Decodable { var books: [ReadingBook] }
        let a: Answer = try await get("/api/books/currently-reading")
        return a.books
    }

    public func updateBook(_ id: String, patch: BookPatch) async throws -> Book {
        var r = try await request("/api/books/\(Self.segment(id))", method: "PATCH")
        r.setValue("application/json", forHTTPHeaderField: "Content-Type")
        r.httpBody = try JSONEncoder().encode(patch)
        return try await send(r)
    }

    /// 204; the object stays in the bucket's 30-day retention.
    public func deleteBook(_ id: String) async throws {
        _ = try await send(raw: try await request("/api/books/\(Self.segment(id))", method: "DELETE"))
    }

    /// The EPUB's bytes: a book streams through the daemon, a doc renders.
    public func epub(_ source: ReadingSource) async throws -> Data {
        var r = try await request(source.filePath)
        r.setValue("application/epub+zip", forHTTPHeaderField: "Accept")
        r.cachePolicy = .reloadIgnoringLocalCacheData
        let (data, response) = try await data(for: r)
        try Self.check(data: data, response: response)
        return data
    }

    public func position(_ source: ReadingSource) async throws -> PositionRead {
        struct Answer: Decodable { var position: ReaderPosition? }
        var r = try await request(source.positionPath)
        r.cachePolicy = .reloadIgnoringLocalCacheData
        do {
            let a: Answer = try await send(r)
            return .available(a.position)
        } catch APIError.notFound {
            return .unavailable
        } catch APIError.notAPIRoute {
            return .unavailable
        }
    }

    /// Never throws: a refusal with the newer row is `.newer`, anything
    /// else that isn't a write is `.failed` (retried with the next move).
    public func putPosition(_ source: ReadingSource, _ body: PositionWrite) async -> PositionWriteResult {
        struct Answer: Decodable { var position: ReaderPosition? }
        do {
            var r = try await request(source.positionPath, method: "PUT")
            r.setValue("application/json", forHTTPHeaderField: "Content-Type")
            r.httpBody = try JSONEncoder().encode(body)
            let (data, response) = try await data(for: r)
            let status = (response as? HTTPURLResponse)?.statusCode ?? 0
            let row = (try? JSONDecoder().decode(Answer.self, from: data))?.position
            if status == 409, let row { return .newer(row) }
            if (200..<300).contains(status), let row { return .written(row) }
            return .failed
        } catch {
            return .failed
        }
    }

    /// The shown cover (or, `embedded`, the book's own). The route 302s to
    /// a presigned object-store URL: the bearer must not follow it there
    /// (the store refuses a request carrying two kinds of auth).
    public func coverImage(_ id: String, version: Int64? = nil, embedded: Bool = false) async throws -> Data {
        var q: [URLQueryItem] = []
        if embedded { q.append(URLQueryItem(name: "which", value: "embedded")) }
        if let version, version > 0 { q.append(URLQueryItem(name: "v", value: String(version))) }
        var r = try await request("/api/books/\(Self.segment(id))/cover", query: q)
        r.setValue("image/*", forHTTPHeaderField: "Accept")
        return try await imageData(r)
    }

    public func coverCandidates(_ id: String) async throws -> CoverCandidates {
        try await get("/api/books/\(Self.segment(id))/cover-candidates")
    }

    /// Ask Open Library (the server does). With a title: the person's own
    /// terms ("Search with a different title…"), always a fresh lookup.
    public func findCoverCandidates(_ id: String, refresh: Bool = false, title: String? = nil, author: String? = nil) async throws -> CoverCandidates {
        struct Body: Encodable {
            var refresh: Bool
            var title: String?
            var author: String?
        }
        let t = title?.trimmingCharacters(in: .whitespacesAndNewlines)
        let body = (t?.isEmpty == false)
            ? Body(refresh: true, title: t, author: author?.trimmingCharacters(in: .whitespacesAndNewlines) ?? "")
            : Body(refresh: refresh)
        return try await post("/api/books/\(Self.segment(id))/cover-candidates", body: body)
    }

    /// A candidate's image, by the server's own path (`candidate.url`).
    public func candidateImage(_ path: String) async throws -> Data {
        guard path.hasPrefix("/api/books/") else { throw APIError.badURL(path) }
        return try await imageData(try await request(path))
    }

    /// Show a candidate, or (nil) the book's own cover, explicitly.
    public func chooseCover(_ id: String, candidate: String?, unlessChosen: Bool = false) async throws -> Book {
        struct Body: Encodable {
            var candidate: String?
            var use: String?
            var unless_chosen: Bool
        }
        let body = candidate.map { Body(candidate: $0, use: nil, unless_chosen: unlessChosen) }
            ?? Body(candidate: nil, use: "embedded", unless_chosen: unlessChosen)
        return try await post("/api/books/\(Self.segment(id))/cover", body: body)
    }

    /// `POST /api/books` (multipart, one `files` field per EPUB).
    public func uploadBooks(_ files: [(name: String, data: Data)]) async throws -> [BookUploadResult] {
        struct Answer: Decodable { var results: [BookUploadResult] }
        let boundary = "taisce-\(UUID().uuidString)"
        var body = Data()
        for f in files {
            let name = f.name.replacingOccurrences(of: "\"", with: "'").replacingOccurrences(of: "\r", with: " ").replacingOccurrences(of: "\n", with: " ")
            body.append(Data("--\(boundary)\r\n".utf8))
            body.append(Data("Content-Disposition: form-data; name=\"files\"; filename=\"\(name)\"\r\n".utf8))
            body.append(Data("Content-Type: application/epub+zip\r\n\r\n".utf8))
            body.append(f.data)
            body.append(Data("\r\n".utf8))
        }
        body.append(Data("--\(boundary)--\r\n".utf8))
        var r = try await request("/api/books", method: "POST")
        r.setValue("multipart/form-data; boundary=\(boundary)", forHTTPHeaderField: "Content-Type")
        r.httpBody = body
        let a: Answer = try await send(r)
        return a.results
    }

    /// An image from one of this server's own book routes, by its full URL
    /// (what `BookCover` is handed): a cover or a candidate. Anything else is refused.
    public func bookImage(at url: URL) async throws -> Data {
        guard url.host() == config.baseURL.host(), url.port == config.baseURL.port,
              let c = URLComponents(url: url, resolvingAgainstBaseURL: false)
        else { throw APIError.badURL(url.absoluteString) }
        let base = config.baseURL.path().hasSuffix("/") ? String(config.baseURL.path().dropLast()) : config.baseURL.path()
        guard c.path.hasPrefix(base + "/api/books/") else { throw APIError.badURL(url.absoluteString) }
        var r = try await request(String(c.path.dropFirst(base.count)), query: c.queryItems ?? [])
        r.setValue("image/*", forHTTPHeaderField: "Accept")
        return try await imageData(r)
    }

    /// The URL `BookCover` loads a book's shown (or own) cover from.
    public func coverURL(_ book: Book, embedded: Bool = false) -> URL? {
        guard book.hasCover || embedded else { return nil }
        var q: [URLQueryItem] = []
        if embedded { q.append(URLQueryItem(name: "which", value: "embedded")) }
        if let v = book.coverVersion, v > 0 { q.append(URLQueryItem(name: "v", value: String(v))) }
        return try? url("/api/books/\(Self.segment(book.id))/cover", query: q)
    }

    /// A candidate's image URL on this server.
    public func candidateURL(_ c: CoverCandidate) -> URL? {
        try? url(c.url)
    }

    // MARK: plumbing

    /// An image round trip whose redirects drop the bearer.
    func imageData(_ request: URLRequest) async throws -> Data {
        let delegate = StripAuthOnRedirect()
        var (data, response) = try await session.data(for: request, delegate: delegate)
        if Self.isUnauthorized(response), let retry = try await renewed(request) {
            (data, response) = try await session.data(for: retry, delegate: delegate)
        }
        if let http = response as? HTTPURLResponse, !(200..<300).contains(http.statusCode) {
            if http.statusCode == 404 { throw APIError.notFound(request.url?.path() ?? "") }
            if http.statusCode == 401 { throw APIError.unauthorized }
            throw APIError.http(status: http.statusCode)
        }
        guard !data.isEmpty, response.mimeType?.hasPrefix("image/") ?? true || response.mimeType == "application/octet-stream" else {
            throw APIError.decoding("not an image")
        }
        return data
    }
}

/// Follows a redirect to another host without the `Authorization` header
/// (a presigned URL carries its own; the bearer must not leak to it).
final class StripAuthOnRedirect: NSObject, URLSessionTaskDelegate, Sendable {
    func urlSession(_ session: URLSession, task: URLSessionTask, willPerformHTTPRedirection response: HTTPURLResponse,
                    newRequest request: URLRequest) async -> URLRequest? {
        var r = request
        let from = task.originalRequest?.url
        if r.url?.host() != from?.host() || r.url?.port != from?.port || r.url?.scheme != from?.scheme {
            r.setValue(nil, forHTTPHeaderField: "Authorization")
        }
        return r
    }
}
