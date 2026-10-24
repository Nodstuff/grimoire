import Foundation

/// A library book as `GET /api/books` serves it (`library.rs` `book_json`).
/// Instants are RFC 3339 strings on the wire; `addedAt` / `updatedAt` parse them.
public struct Book: Codable, Sendable, Hashable, Identifiable {
    public var id: String
    public var title: String
    /// "" or absent when the book has none
    public var author: String?
    public var series: String?
    public var seriesIndex: Double?
    public var bookIdentifier: String?
    public var originalFilename: String
    public var size: Int64
    /// a cover to show: the colour one while chosen, else the book's own
    public var hasCover: Bool
    public var hasEmbeddedCover: Bool?
    public var hasColorCover: Bool?
    /// the book's own cover: true = black and white, false = colour, nil = not looked at
    public var coverGreyscale: Bool?
    /// who decided which cover shows: "none" | "embedded" | "color"
    public var coverChoice: String?
    /// moves whenever the shown cover does (cache-buster)
    public var coverVersion: Int64?
    public var added: String
    public var updated: String

    public init(id: String, title: String, author: String? = nil, series: String? = nil, seriesIndex: Double? = nil,
                bookIdentifier: String? = nil, originalFilename: String = "", size: Int64 = 0, hasCover: Bool = false,
                hasEmbeddedCover: Bool? = nil, hasColorCover: Bool? = nil, coverGreyscale: Bool? = nil,
                coverChoice: String? = nil, coverVersion: Int64? = nil, added: String = "", updated: String = "") {
        self.id = id
        self.title = title
        self.author = author
        self.series = series
        self.seriesIndex = seriesIndex
        self.bookIdentifier = bookIdentifier
        self.originalFilename = originalFilename
        self.size = size
        self.hasCover = hasCover
        self.hasEmbeddedCover = hasEmbeddedCover
        self.hasColorCover = hasColorCover
        self.coverGreyscale = coverGreyscale
        self.coverChoice = coverChoice
        self.coverVersion = coverVersion
        self.added = added
        self.updated = updated
    }

    enum CodingKeys: String, CodingKey {
        case id, title, author, series, size
        case seriesIndex = "series_index"
        case bookIdentifier = "book_identifier"
        case originalFilename = "original_filename"
        case hasCover = "has_cover"
        case hasEmbeddedCover = "has_embedded_cover"
        case hasColorCover = "has_color_cover"
        case coverGreyscale = "cover_greyscale"
        case coverChoice = "cover_choice"
        case coverVersion = "cover_version"
        case added = "added_at"
        case updated = "updated_at"
    }

    /// The author to show, or nil when there is none.
    public var authorName: String? {
        guard let a = author?.trimmingCharacters(in: .whitespacesAndNewlines), !a.isEmpty else { return nil }
        return a
    }

    public var seriesName: String? {
        guard let s = series?.trimmingCharacters(in: .whitespacesAndNewlines), !s.isEmpty else { return nil }
        return s
    }

    public var addedAt: Date? { RFC3339.date(added) }

    /// The colour cover is showing (chosen from Open Library).
    public var showsColorCover: Bool { hasColorCover == true && coverChoice == "color" }

    /// A colour match is worth offering: a black-and-white cover nobody chose.
    public var wantsColourCover: Bool {
        coverGreyscale == true && (coverChoice ?? "none") == "none" && hasColorCover != true
    }

    /// The book has no cover at all and nobody chose "no cover".
    public var wantsCover: Bool {
        !hasCover && hasEmbeddedCover != true && hasColorCover != true && (coverChoice ?? "none") == "none"
    }

    /// The name the feed and the share sheet give the file: `Author - Title.epub`.
    public var downloadFilename: String {
        let base = authorName.map { "\($0) - \(title)" } ?? title
        let safe = base.replacingOccurrences(of: "/", with: "-").replacingOccurrences(of: ":", with: " ")
        return safe.trimmingCharacters(in: .whitespacesAndNewlines) + ".epub"
    }
}

public enum ProgressStatus: String, Codable, Sendable, Hashable {
    case unread, reading, finished
}

/// `GET /api/books/progress`: the newest kosync row for a book, scaled for
/// people (`percentage` 0–100, `updated_at` RFC 3339).
public struct BookProgress: Codable, Sendable, Hashable {
    public var percentage: Double
    public var device: String
    public var updated: String
    public var status: ProgressStatus

    public init(percentage: Double, device: String, updated: String, status: ProgressStatus) {
        self.percentage = percentage
        self.device = device
        self.updated = updated
        self.status = status
    }

    enum CodingKeys: String, CodingKey {
        case percentage, device, status
        case updated = "updated_at"
    }

    public var updatedAt: Date? { RFC3339.date(updated) }
}

/// A `GET /api/books/currently-reading` row: the book with its progress
/// flattened in (`updated_at` is the PROGRESS stamp there).
public struct ReadingBook: Decodable, Sendable, Hashable {
    public var book: Book
    public var progress: BookProgress

    public init(book: Book, progress: BookProgress) {
        self.book = book
        self.progress = progress
    }

    public init(from decoder: any Decoder) throws {
        book = try Book(from: decoder)
        progress = try BookProgress(from: decoder)
    }
}

/// A page of the library, newest first.
public struct BookPage: Decodable, Sendable, Hashable {
    public var books: [Book]
    public var nextBefore: String?

    enum CodingKeys: String, CodingKey {
        case books
        case nextBefore = "next_before"
    }
}

/// An e-reader (OPDS + KOReader sync) set up in Settings › E-readers.
public struct Ereader: Codable, Sendable, Hashable, Identifiable {
    public var id: String
    public var label: String
    public var lastUsed: String?
    public var revoked: String?

    public init(id: String, label: String, lastUsed: String? = nil, revoked: String? = nil) {
        self.id = id
        self.label = label
        self.lastUsed = lastUsed
        self.revoked = revoked
    }

    enum CodingKeys: String, CodingKey {
        case id, label
        case lastUsed = "last_used_at"
        case revoked = "revoked_at"
    }

    public var isActive: Bool { revoked == nil }
}

/// `PATCH /api/books/{id}`: absent fields stay, `""` / null clear.
public struct BookPatch: Encodable, Sendable, Hashable {
    public var title: String?
    /// "" clears the author
    public var author: String?
    /// .some(nil) clears
    public var series: String??
    public var seriesIndex: Double??

    public init(title: String? = nil, author: String? = nil, series: String?? = nil, seriesIndex: Double?? = nil) {
        self.title = title
        self.author = author
        self.series = series
        self.seriesIndex = seriesIndex
    }

    enum CodingKeys: String, CodingKey {
        case title, author, series
        case seriesIndex = "series_index"
    }

    public func encode(to encoder: any Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encodeIfPresent(title, forKey: .title)
        try c.encodeIfPresent(author, forKey: .author)
        if let series {
            if let s = series { try c.encode(s, forKey: .series) } else { try c.encodeNil(forKey: .series) }
        }
        if let seriesIndex {
            if let n = seriesIndex { try c.encode(n, forKey: .seriesIndex) } else { try c.encodeNil(forKey: .seriesIndex) }
        }
    }

    /// The edits a metadata form makes to `book`: only what changed.
    /// A series # that isn't a number throws `BookPatch.Invalid`.
    public static func edits(from book: Book, title: String, author: String, series: String, seriesIndex: String) throws -> BookPatch {
        var p = BookPatch()
        let t = title.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !t.isEmpty else { throw Invalid.emptyTitle }
        if t != book.title { p.title = t }
        let a = author.trimmingCharacters(in: .whitespacesAndNewlines)
        if a != (book.authorName ?? "") { p.author = a }
        let s = series.trimmingCharacters(in: .whitespacesAndNewlines)
        if s != (book.seriesName ?? "") { p.series = .some(s.isEmpty ? nil : s) }
        let i = seriesIndex.trimmingCharacters(in: .whitespacesAndNewlines)
        let n: Double?
        if i.isEmpty {
            n = nil
        } else if let v = Double(i.replacingOccurrences(of: ",", with: ".")), v.isFinite, v >= 0 {
            n = v
        } else {
            throw Invalid.seriesIndex
        }
        if n != book.seriesIndex { p.seriesIndex = .some(n) }
        return p
    }

    public var isEmpty: Bool { title == nil && author == nil && series == nil && seriesIndex == nil }

    public enum Invalid: Error, Equatable, LocalizedError {
        case emptyTitle, seriesIndex
        public var errorDescription: String? {
            switch self {
            case .emptyTitle: "A book needs a title."
            case .seriesIndex: "The series number must be a number, like 3 or 2.5."
            }
        }
    }
}

/// A colour cover the server found on Open Library and validated. `url` is
/// the server's own route to the image (clients never fetch Open Library).
public struct CoverCandidate: Codable, Sendable, Hashable, Identifiable {
    public var id: String
    /// "isbn" (this edition) | "search" (title match)
    public var source: String
    public var width: Int
    public var height: Int
    public var url: String

    public init(id: String, source: String, width: Int, height: Int, url: String) {
        self.id = id
        self.source = source
        self.width = width
        self.height = height
        self.url = url
    }

    public var sourceLabel: String { source == "isbn" ? "This edition" : "Title match" }
}

public struct CoverCandidates: Decodable, Sendable, Hashable {
    public var book: Book
    public var lookedUpAt: String?
    public var candidates: [CoverCandidate]
    /// Open Library did not answer: try again later
    public var unavailable: Bool

    enum CodingKeys: String, CodingKey {
        case book, candidates, unavailable
        case lookedUpAt = "looked_up_at"
    }
}

/// One file's outcome from `POST /api/books`.
public struct BookUploadResult: Decodable, Sendable, Hashable {
    public enum Status: String, Decodable, Sendable {
        case added, updated, clash, error
        case alreadyHere = "already_here"
    }

    public var filename: String
    public var status: Status
    public var book: Book?
    public var message: String?
}

/// The `Book` this item is read from: a library book, or a doc rendered as
/// an EPUB (`GET /api/docs/{id}/epub`).
public enum ReadingSource: Sendable, Hashable, Codable {
    case book(String)
    case doc(String)

    public var id: String {
        switch self {
        case let .book(id), let .doc(id): id
        }
    }

    /// The path the EPUB comes from.
    public var filePath: String {
        switch self {
        case let .book(id): "/api/books/\(id)/file"
        case let .doc(id): "/api/docs/\(id)/epub"
        }
    }

    public var positionPath: String {
        switch self {
        case let .book(id): "/api/books/\(id)/position"
        case let .doc(id): "/api/docs/\(id)/position"
        }
    }

    /// A stable key for per-item local state (`book.<id>` / `doc.<id>`).
    public var key: String {
        switch self {
        case let .book(id): "book.\(id)"
        case let .doc(id): "doc.\(id)"
        }
    }
}

/// RFC 3339 instants as the server writes them (with or without fractions).
public enum RFC3339 {
    public static func date(_ s: String) -> Date? {
        guard !s.isEmpty else { return nil }
        if let d = try? Date(s, strategy: .iso8601) { return d }
        let f = ISO8601DateFormatter()
        f.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return f.date(from: s)
    }
}
