import Foundation

/// What a share link publishes: the doc's markdown with each visual block
/// swapped for `![alt](taisce-asset:<name>)`, and those images. `pages` are
/// the doc's sub-pages (its descendants), when the link carries them: their
/// images sit in the same `assets` list under per-page names.
public struct ShareSnapshot: Codable, Sendable, Hashable {
    public enum Theme: String, Codable, Sendable, Hashable {
        case light, dark, auto
    }

    public var title: String
    public var markdown: String
    public var assets: [ShareAsset]
    public var theme: Theme
    /// the root's descendants in tree order (a parent before its children,
    /// siblings as the sidebar shows them); empty = a single-doc link
    public var pages: [SharePage]

    public init(title: String, markdown: String, assets: [ShareAsset] = [], theme: Theme, pages: [SharePage] = []) {
        self.title = title
        self.markdown = markdown
        self.assets = assets
        self.theme = theme
        self.pages = pages
    }

    enum CodingKeys: String, CodingKey {
        case title, markdown, assets, theme, pages
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        title = try c.decode(String.self, forKey: .title)
        markdown = try c.decode(String.self, forKey: .markdown)
        assets = try c.decodeIfPresent([ShareAsset].self, forKey: .assets) ?? []
        theme = try c.decode(Theme.self, forKey: .theme)
        pages = try c.decodeIfPresent([SharePage].self, forKey: .pages) ?? []
    }

    /// `pages` is sent only when there are any: a single-doc request is
    /// what it always was.
    public func encode(to encoder: any Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(title, forKey: .title)
        try c.encode(markdown, forKey: .markdown)
        try c.encode(assets, forKey: .assets)
        try c.encode(theme, forKey: .theme)
        if !pages.isEmpty { try c.encode(pages, forKey: .pages) }
    }

    /// The bytes the server counts against its 10 MB limit (roughly: all
    /// the markdown plus each asset's base64).
    public var approximateSize: Int {
        markdown.utf8.count + pages.reduce(0) { $0 + $1.markdown.utf8.count } + assets.reduce(0) { $0 + ($1.data.count + 2) / 3 * 4 }
    }
}

/// One sub-page of a link: a descendant of the shared doc. Its images are
/// in the snapshot's `assets`, referenced as `taisce-asset:<name>`.
public struct SharePage: Codable, Sendable, Hashable {
    public var docID: DocID
    /// the page's parent doc: the root's id (or nil) for a direct child
    public var parentDocID: DocID?
    public var title: String
    public var markdown: String

    public init(docID: DocID, parentDocID: DocID? = nil, title: String, markdown: String) {
        self.docID = docID
        self.parentDocID = parentDocID
        self.title = title
        self.markdown = markdown
    }

    enum CodingKeys: String, CodingKey {
        case title, markdown
        case docID = "doc_id"
        case parentDocID = "parent_doc_id"
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        docID = try c.decode(DocID.self, forKey: .docID)
        parentDocID = try c.decodeIfPresent(DocID.self, forKey: .parentDocID)
        title = try c.decodeIfPresent(String.self, forKey: .title) ?? ""
        markdown = try c.decodeIfPresent(String.self, forKey: .markdown) ?? ""
    }

    /// `parent_doc_id` is always there (null for a direct child without one).
    public func encode(to encoder: any Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(docID, forKey: .docID)
        try c.encode(parentDocID, forKey: .parentDocID)
        try c.encode(title, forKey: .title)
        try c.encode(markdown, forKey: .markdown)
    }
}

/// One image in a snapshot. `data` travels as base64 (JSONEncoder's default for `Data`).
public struct ShareAsset: Codable, Sendable, Hashable {
    public var name: String
    public var contentType: String
    public var data: Data
    public var width: Int?
    public var height: Int?

    public init(name: String, contentType: String, data: Data, width: Int? = nil, height: Int? = nil) {
        self.name = name
        self.contentType = contentType
        self.data = data
        self.width = width
        self.height = height
    }

    enum CodingKeys: String, CodingKey {
        case name, data, width, height
        case contentType = "content_type"
    }

    public static let allowedTypes: Set<String> = ["image/svg+xml", "image/png", "image/jpeg", "image/webp"]

    /// `[A-Za-z0-9._-]{1,64}`, as the server checks.
    public static func isValidName(_ name: String) -> Bool {
        (1...64).contains(name.count) && name.allSatisfy { $0.isASCII && ($0.isLetter || $0.isNumber || $0 == "." || $0 == "_" || $0 == "-") }
    }
}

/// The server's limits on a snapshot (413 past any of them).
public enum ShareLimits {
    public static let snapshotBytes = 10 * 1024 * 1024
    public static let assetBytes = 2 * 1024 * 1024
    /// images per link, across all its pages
    public static let assets = 1000
    /// per page
    public static let markdownBytes = 2 * 1024 * 1024
    /// sub-pages per link
    public static let pages = 200
}

/// A published link (`GET /api/shares`).
public struct Share: Codable, Sendable, Hashable, Identifiable {
    public var id: String
    public var docID: DocID
    public var url: String
    public var createdAt: Date?
    public var updatedAt: Date?
    public var expiresAt: Date?
    public var revokedAt: Date?
    public var commentsEnabled: Bool
    public var revision: Int
    public var views: Int
    public var lastViewedAt: Date?
    public var commentCount: Int
    public var unreadComments: Int
    public var title: String
    /// how many sub-pages the link carries (0 = the doc alone)
    public var pageCount: Int

    public init(
        id: String, docID: DocID, url: String, createdAt: Date? = nil, updatedAt: Date? = nil, expiresAt: Date? = nil,
        revokedAt: Date? = nil, commentsEnabled: Bool = true, revision: Int = 1, views: Int = 0, lastViewedAt: Date? = nil,
        commentCount: Int = 0, unreadComments: Int = 0, title: String = "", pageCount: Int = 0
    ) {
        self.id = id
        self.docID = docID
        self.url = url
        self.createdAt = createdAt
        self.updatedAt = updatedAt
        self.expiresAt = expiresAt
        self.revokedAt = revokedAt
        self.commentsEnabled = commentsEnabled
        self.revision = revision
        self.views = views
        self.lastViewedAt = lastViewedAt
        self.commentCount = commentCount
        self.unreadComments = unreadComments
        self.title = title
        self.pageCount = pageCount
    }

    enum CodingKeys: String, CodingKey {
        case id, url, revision, views, title
        case pageCount = "pages"
        case docID = "doc_id"
        case createdAt = "created_at"
        case updatedAt = "updated_at"
        case expiresAt = "expires_at"
        case revokedAt = "revoked_at"
        case commentsEnabled = "comments_enabled"
        case lastViewedAt = "last_viewed_at"
        case commentCount = "comment_count"
        case unreadComments = "unread_comments"
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        docID = try c.decode(DocID.self, forKey: .docID)
        url = try c.decode(String.self, forKey: .url)
        createdAt = try ShareDate.decode(c, .createdAt)
        updatedAt = try ShareDate.decode(c, .updatedAt)
        expiresAt = try ShareDate.decode(c, .expiresAt)
        revokedAt = try ShareDate.decode(c, .revokedAt)
        lastViewedAt = try ShareDate.decode(c, .lastViewedAt)
        commentsEnabled = try c.decodeIfPresent(Bool.self, forKey: .commentsEnabled) ?? true
        revision = try c.decodeIfPresent(Int.self, forKey: .revision) ?? 1
        views = try c.decodeIfPresent(Int.self, forKey: .views) ?? 0
        commentCount = try c.decodeIfPresent(Int.self, forKey: .commentCount) ?? 0
        unreadComments = try c.decodeIfPresent(Int.self, forKey: .unreadComments) ?? 0
        title = try c.decodeIfPresent(String.self, forKey: .title) ?? ""
        pageCount = max(0, try c.decodeIfPresent(Int.self, forKey: .pageCount) ?? 0)
    }

    public func encode(to encoder: any Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(id, forKey: .id)
        try c.encode(docID, forKey: .docID)
        try c.encode(url, forKey: .url)
        try c.encode(createdAt.map(ShareDate.string), forKey: .createdAt)
        try c.encode(updatedAt.map(ShareDate.string), forKey: .updatedAt)
        try c.encode(expiresAt.map(ShareDate.string), forKey: .expiresAt)
        try c.encode(revokedAt.map(ShareDate.string), forKey: .revokedAt)
        try c.encode(commentsEnabled, forKey: .commentsEnabled)
        try c.encode(revision, forKey: .revision)
        try c.encode(views, forKey: .views)
        try c.encode(lastViewedAt.map(ShareDate.string), forKey: .lastViewedAt)
        try c.encode(commentCount, forKey: .commentCount)
        try c.encode(unreadComments, forKey: .unreadComments)
        try c.encode(title, forKey: .title)
        try c.encode(pageCount, forKey: .pageCount)
    }

    public enum State: Sendable, Hashable {
        case active, expired, revoked
    }

    public func state(now: Date = .now) -> State {
        if revokedAt != nil { return .revoked }
        if let expiresAt, expiresAt <= now { return .expired }
        return .active
    }
}

/// Where a comment points: the page's top-level block index and the text the reader selected.
public struct ShareAnchor: Codable, Sendable, Hashable {
    public var block: Int?
    public var quote: String?

    public init(block: Int? = nil, quote: String? = nil) {
        self.block = block
        self.quote = quote
    }
}

/// A comment on a share link: a reader's (by name) or the owner's reply.
public struct ShareComment: Codable, Sendable, Hashable, Identifiable {
    public var id: String
    public var parentID: String?
    public var author: String
    public var isOwner: Bool
    public var body: String
    public var anchor: ShareAnchor?
    public var createdAt: Date?
    public var revision: Int?
    /// which page of the link it is on (an opaque slug); nil = the root page
    public var page: String?
    /// that page's title; nil with a `page` = a page a later snapshot removed
    public var pageTitle: String?

    public init(
        id: String, parentID: String? = nil, author: String, isOwner: Bool = false, body: String, anchor: ShareAnchor? = nil,
        createdAt: Date? = nil, revision: Int? = nil, page: String? = nil, pageTitle: String? = nil
    ) {
        self.id = id
        self.parentID = parentID
        self.author = author
        self.isOwner = isOwner
        self.body = body
        self.anchor = anchor
        self.createdAt = createdAt
        self.revision = revision
        self.page = page
        self.pageTitle = pageTitle
    }

    /// "on Fleet Twin" for a comment on a sub-page ("on a removed page" when
    /// a later snapshot dropped it); nil on the root page.
    public var pageCaption: String? {
        guard let page, !page.isEmpty else { return nil }
        guard let pageTitle, !pageTitle.isEmpty else { return "on a removed page" }
        return "on \(pageTitle)"
    }

    enum CodingKeys: String, CodingKey {
        case id, author, body, anchor, revision, page
        case pageTitle = "page_title"
        case parentID = "parent_id"
        case isOwner = "is_owner"
        case createdAt = "created_at"
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        parentID = try c.decodeIfPresent(String.self, forKey: .parentID)
        author = try c.decodeIfPresent(String.self, forKey: .author) ?? ""
        isOwner = try c.decodeIfPresent(Bool.self, forKey: .isOwner) ?? false
        body = try c.decodeIfPresent(String.self, forKey: .body) ?? ""
        anchor = try c.decodeIfPresent(ShareAnchor.self, forKey: .anchor)
        createdAt = try ShareDate.decode(c, .createdAt)
        revision = try c.decodeIfPresent(Int.self, forKey: .revision)
        page = try c.decodeIfPresent(String.self, forKey: .page)
        pageTitle = try c.decodeIfPresent(String.self, forKey: .pageTitle)
    }

    public func encode(to encoder: any Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(id, forKey: .id)
        try c.encodeIfPresent(parentID, forKey: .parentID)
        try c.encode(author, forKey: .author)
        try c.encode(isOwner, forKey: .isOwner)
        try c.encode(body, forKey: .body)
        try c.encodeIfPresent(anchor, forKey: .anchor)
        try c.encodeIfPresent(createdAt.map(ShareDate.string), forKey: .createdAt)
        try c.encodeIfPresent(revision, forKey: .revision)
        try c.encodeIfPresent(page, forKey: .page)
        try c.encodeIfPresent(pageTitle, forKey: .pageTitle)
    }
}

/// A root comment and its replies, oldest first.
public struct ShareThread: Sendable, Hashable, Identifiable {
    public var root: ShareComment
    public var replies: [ShareComment]
    public var id: String { root.id }

    /// Roots in the order they came; a reply whose parent is gone (deleted)
    /// becomes a root of its own rather than vanishing. Replies to replies
    /// join the thread of their root.
    public static func group(_ comments: [ShareComment]) -> [ShareThread] {
        let byID = Dictionary(comments.map { ($0.id, $0) }, uniquingKeysWith: { a, _ in a })
        func rootID(_ c: ShareComment) -> String {
            var cur = c
            var seen: Set<String> = [c.id]
            while let p = cur.parentID, let parent = byID[p], seen.insert(p).inserted { cur = parent }
            return cur.id
        }
        var order: [String] = []
        var threads: [String: ShareThread] = [:]
        for c in comments where c.parentID == nil || byID[c.parentID!] == nil {
            order.append(c.id)
            threads[c.id] = ShareThread(root: c, replies: [])
        }
        for c in comments where c.parentID != nil && byID[c.parentID!] != nil {
            threads[rootID(c)]?.replies.append(c)
        }
        func when(_ c: ShareComment) -> Date { c.createdAt ?? .distantPast }
        return order.compactMap { id in
            threads[id].map { t in
                var t = t
                t.replies.sort { when($0) < when($1) }
                return t
            }
        }
    }
}

/// RFC 3339 on the wire, with or without fractional seconds.
public enum ShareDate {
    public static func parse(_ s: String) -> Date? {
        let frac = Date.ISO8601FormatStyle(includingFractionalSeconds: true)
        if let d = try? frac.parse(s) { return d }
        return try? Date.ISO8601FormatStyle().parse(s)
    }

    public static func string(_ d: Date) -> String {
        d.formatted(Date.ISO8601FormatStyle())
    }

    static func decode<K: CodingKey>(_ c: KeyedDecodingContainer<K>, _ key: K) throws -> Date? {
        guard let s = try c.decodeIfPresent(String.self, forKey: key) else { return nil }
        guard let d = parse(s) else {
            throw DecodingError.dataCorruptedError(forKey: key, in: c, debugDescription: "not an RFC 3339 date: \(s)")
        }
        return d
    }
}

/// How long a new link lives. Default: 7 days.
public enum ShareExpiry: Sendable, Hashable, CaseIterable, Identifiable {
    case hour, day, week, month, custom, never

    public static let `default` = ShareExpiry.week
    public var id: Self { self }

    public var title: String {
        switch self {
        case .hour: "1 hour"
        case .day: "1 day"
        case .week: "7 days"
        case .month: "30 days"
        case .custom: "Custom date"
        case .never: "Never"
        }
    }

    /// The `expires_at` to send: nil = never. `custom` uses `customDate`.
    public func date(from now: Date = .now, custom customDate: Date? = nil) -> Date? {
        switch self {
        case .hour: now.addingTimeInterval(3600)
        case .day: now.addingTimeInterval(86400)
        case .week: now.addingTimeInterval(7 * 86400)
        case .month: now.addingTimeInterval(30 * 86400)
        case .custom: customDate ?? now.addingTimeInterval(7 * 86400)
        case .never: nil
        }
    }
}

/// A custom expiry too close to now (by the time the snapshot was built and sent).
public struct ShareExpiryTooSoon: Error, Sendable, Hashable, LocalizedError {
    public init() {}
    public var errorDescription: String? { "Pick an expiry at least 5 minutes from now." }
}

extension ShareExpiry {
    /// A custom date must be at least this far ahead when the request goes out.
    public static let minimumLead: TimeInterval = 5 * 60

    /// The `expires_at` to send now (nil = never), checked: a custom date
    /// less than `minimumLead` away throws. Call it after the build, just
    /// before the request, so a slow build cannot send a date in the past.
    public func resolve(now: Date = .now, custom: Date?) throws -> Date? {
        let d = date(from: now, custom: custom)
        if self == .custom, let d, d < now.addingTimeInterval(Self.minimumLead) { throw ShareExpiryTooSoon() }
        return d
    }
}
