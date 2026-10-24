import Foundation

/// A book in progress, for Today's "Continue reading" card
/// (`GET /api/books/currently-reading`, SERVER mode; newest progress first).
/// The Books screens have their own, fuller types; this is the card's slice.
public struct TodayBook: Codable, Sendable, Hashable, Identifiable {
    public var id: String
    public var title: String
    public var author: String
    public var series: String?
    public var seriesIndex: Double?
    public var hasCover: Bool
    public var coverVersion: Int64
    /// 0–100
    public var percentage: Double
    /// the device that last reported progress ("Kindle", "Taisce web")
    public var device: String?
    /// RFC 3339: when that progress was stamped
    public var updatedAt: String?

    public init(id: String, title: String, author: String, series: String? = nil, seriesIndex: Double? = nil, hasCover: Bool = false, coverVersion: Int64 = 0, percentage: Double, device: String? = nil, updatedAt: String? = nil) {
        self.id = id
        self.title = title
        self.author = author
        self.series = series
        self.seriesIndex = seriesIndex
        self.hasCover = hasCover
        self.coverVersion = coverVersion
        self.percentage = percentage
        self.device = device
        self.updatedAt = updatedAt
    }

    enum CodingKeys: String, CodingKey {
        case id, title, author, series, percentage, device
        case seriesIndex = "series_index"
        case hasCover = "has_cover"
        case coverVersion = "cover_version"
        case updatedAt = "updated_at"
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        title = try c.decodeIfPresent(String.self, forKey: .title) ?? ""
        author = try c.decodeIfPresent(String.self, forKey: .author) ?? ""
        series = try c.decodeIfPresent(String.self, forKey: .series)
        seriesIndex = try c.decodeIfPresent(Double.self, forKey: .seriesIndex)
        hasCover = try c.decodeIfPresent(Bool.self, forKey: .hasCover) ?? false
        coverVersion = try c.decodeIfPresent(Int64.self, forKey: .coverVersion) ?? 0
        percentage = try c.decodeIfPresent(Double.self, forKey: .percentage) ?? 0
        device = try c.decodeIfPresent(String.self, forKey: .device)
        updatedAt = try c.decodeIfPresent(String.self, forKey: .updatedAt)
    }

    /// The cover's path on the server (nil without one); the version busts caches.
    public var coverPath: String? {
        hasCover ? "/api/books/\(id)/cover?v=\(coverVersion)" : nil
    }

    /// "Book 14" from the series index ("Book 2.5" for halves).
    public var seriesLabel: String? {
        guard let i = seriesIndex else { return nil }
        let n = i == i.rounded() ? String(Int(i)) : String(i)
        return "Book \(n)"
    }
}

/// `POST /api/ask`: the answer doc the server wrote under Answers, or
/// nothing when no doc could ground one.
public struct AskAnswer: Codable, Sendable, Hashable {
    /// nil: nothing in the vault matched (no doc was made)
    public var docID: DocID?
    public var title: String
    /// cited blocks
    public var sources: Int
    /// docs they came from
    public var docs: Int

    public init(docID: DocID?, title: String, sources: Int, docs: Int) {
        self.docID = docID
        self.title = title
        self.sources = sources
        self.docs = docs
    }

    enum CodingKeys: String, CodingKey {
        case title, sources, docs
        case docID = "doc_id"
    }
}

extension APIClient {
    /// Books with progress, newest first. LOCAL daemons have no books (404).
    public func todayReading() async throws -> [TodayBook] {
        struct Body: Decodable { var books: [TodayBook] }
        let b: Body = try await get("/api/books/currently-reading")
        return b.books
    }

    /// Books whose title, author or series match (`GET /api/books?search=`,
    /// first page). Rows without progress read 0%.
    public func searchBooks(_ query: String, limit: Int = 30) async throws -> [TodayBook] {
        struct Body: Decodable { var books: [TodayBook] }
        let b: Body = try await get("/api/books", query: [
            URLQueryItem(name: "search", value: query),
            URLQueryItem(name: "limit", value: String(limit)),
        ])
        return b.books
    }

    /// The book's cover image bytes (the server's colour cover), with this
    /// client's credentials (an image view's own fetch would carry none).
    public func todayBookCover(_ book: TodayBook) async throws -> Data? {
        guard book.hasCover else { return nil }
        let q = [URLQueryItem(name: "v", value: String(book.coverVersion))]
        return try await send(raw: try await request("/api/books/\(book.id)/cover", query: q))
    }

    /// Ask the vault. Waits for the model (up to two minutes).
    public func ask(_ question: String) async throws -> AskAnswer {
        struct Body: Encodable { var question: String }
        var r = try await request("/api/ask", method: "POST")
        r.setValue("application/json", forHTTPHeaderField: "Content-Type")
        r.httpBody = try JSONEncoder().encode(Body(question: question))
        r.timeoutInterval = 150
        return try await send(r)
    }
}
