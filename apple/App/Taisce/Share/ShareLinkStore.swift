import Foundation
import Observation
import SwiftUI
import TaisceKit

/// Your share links and their comments, per doc and in all. SERVER-mode
/// accounts only (`isAvailable`): LOCAL daemons answer every route 404.
@MainActor @Observable
final class ShareLinkStore {
    @ObservationIgnored weak var app: AppModel?
    /// tests: a fake server and a fixed snapshot
    @ObservationIgnored var serviceOverride: (any ShareService)?
    @ObservationIgnored var snapshotOverride: ((DocID, ShareSnapshot.Theme) async throws -> ShareSnapshotResult)?

    private(set) var byDoc: [DocID: [Share]] = [:]
    private(set) var all: [Share] = []
    private(set) var allLoaded = false
    /// what the last publish left out or couldn't draw
    private(set) var lastProblems: [String] = []

    var isAvailable: Bool {
        serviceOverride != nil || app?.authPhase == .signedIn
    }

    private var service: any ShareService {
        get throws {
            if let serviceOverride { return serviceOverride }
            guard let app, app.authPhase == .signedIn, let api = app.api else { throw ShareLinkError.notAvailable }
            return api
        }
    }

    func reset() {
        byDoc = [:]
        all = []
        allLoaded = false
        lastProblems = []
    }

    // MARK: reading

    func shares(for doc: DocID) -> [Share] { byDoc[doc] ?? [] }

    func load(doc: DocID) async throws {
        let list = try await service.shares(docID: doc)
        byDoc[doc] = Self.sorted(list)
        for s in list { merge(intoAll: s) }
    }

    func loadAll() async throws {
        let list = try await service.shares(docID: nil)
        all = Self.sorted(list)
        allLoaded = true
        byDoc = Dictionary(grouping: all, by: \.docID).mapValues(Self.sorted)
    }

    /// Comments across a doc's links: how many, how many new.
    func commentSummary(for doc: DocID) -> LinkCommentSummary {
        LinkCommentSummary(shares(for: doc))
    }

    var unreadTotal: Int { all.reduce(0) { $0 + $1.unreadComments } }

    // MARK: publishing

    func create(doc: DocID, expiresAt: Date?, commentsEnabled: Bool, theme: ShareSnapshot.Theme) async throws -> Share {
        let built = try await snapshot(doc, theme)
        lastProblems = built.problems
        let share = try await service.createShare(docID: doc, snapshot: built.snapshot, expiresAt: expiresAt, commentsEnabled: commentsEnabled)
        apply(share)
        return share
    }

    /// Update link: the doc as it is now, at the same URL.
    func republish(_ share: Share, theme: ShareSnapshot.Theme) async throws {
        let built = try await snapshot(share.docID, theme)
        lastProblems = built.problems
        apply(try await service.updateShare(share.id, snapshot: built.snapshot, expiresAt: .keep, commentsEnabled: nil))
    }

    func setComments(_ share: Share, enabled: Bool) async throws {
        apply(try await service.updateShare(share.id, snapshot: nil, expiresAt: .keep, commentsEnabled: enabled))
    }

    func setExpiry(_ share: Share, to date: Date?) async throws {
        apply(try await service.updateShare(share.id, snapshot: nil, expiresAt: .set(date), commentsEnabled: nil))
    }

    func revoke(_ share: Share) async throws {
        try await service.revokeShare(share.id)
        var s = share
        s.revokedAt = .now
        apply(s)
    }

    // MARK: comments

    /// Reading them marks them read (server side; mirrored here).
    func comments(for share: Share) async throws -> [ShareComment] {
        let list = try await service.shareComments(share.id)
        var s = share
        s.unreadComments = 0
        s.commentCount = list.count
        apply(s)
        return list
    }

    func reply(to share: Share, body: String, parent: ShareComment?) async throws -> ShareComment {
        let text = body.trimmingCharacters(in: .whitespacesAndNewlines)
        let c = try await service.replyToShareComment(share.id, body: text, parentID: parent?.id, anchor: nil)
        var s = share
        s.commentCount += 1
        apply(s)
        return c
    }

    func delete(_ comment: ShareComment, on share: Share) async throws {
        try await service.deleteShareComment(share.id, commentID: comment.id)
        var s = share
        s.commentCount = max(0, s.commentCount - 1)
        apply(s)
    }

    // MARK: plumbing

    private func snapshot(_ doc: DocID, _ theme: ShareSnapshot.Theme) async throws -> ShareSnapshotResult {
        if let snapshotOverride { return try await snapshotOverride(doc, theme) }
        guard let app else { throw ShareLinkError.notAvailable }
        return try await app.shareSnapshot(doc, theme: theme)
    }

    /// A share the server answered with (or changed here) replaces its row everywhere.
    func apply(_ share: Share) {
        var list = byDoc[share.docID] ?? []
        if let i = list.firstIndex(where: { $0.id == share.id }) { list[i] = share } else { list.append(share) }
        byDoc[share.docID] = Self.sorted(list)
        merge(intoAll: share)
    }

    private func merge(intoAll share: Share) {
        if let i = all.firstIndex(where: { $0.id == share.id }) {
            all[i] = share
        } else if allLoaded {
            all = Self.sorted(all + [share])
        }
    }

    /// Live links first, newest first within each group.
    static func sorted(_ list: [Share]) -> [Share] {
        list.sorted { a, b in
            let la = a.state() == .active, lb = b.state() == .active
            if la != lb { return la }
            return (a.createdAt ?? .distantPast) > (b.createdAt ?? .distantPast)
        }
    }
}

/// "Comments from shared links (N new)" on a doc.
struct LinkCommentSummary: Equatable {
    var total: Int
    var unread: Int

    init(total: Int, unread: Int) {
        self.total = total
        self.unread = unread
    }

    init(_ shares: [Share]) {
        total = shares.reduce(0) { $0 + $1.commentCount }
        unread = shares.reduce(0) { $0 + $1.unreadComments }
    }

    /// nil when there is nothing to show
    var title: String? {
        guard total > 0 || unread > 0 else { return nil }
        return unread > 0 ? "Comments from shared links (\(unread) new)" : "Comments from shared links"
    }
}

/// Words for a link's state and numbers.
enum ShareLinkText {
    static func state(_ share: Share, now: Date = .now) -> String {
        switch share.state(now: now) {
        case .revoked: "Revoked"
        case .expired: "Expired"
        case .active: share.expiresAt.map { "Expires \(expiry($0, now: now))" } ?? "Never expires"
        }
    }

    static func expiry(_ date: Date, now: Date = .now) -> String {
        let calendar = Calendar.current
        if calendar.isDate(date, inSameDayAs: now) { return "today at \(date.formatted(date: .omitted, time: .shortened))" }
        return date.formatted(date: .abbreviated, time: .shortened)
    }

    static func views(_ share: Share, now: Date = .now) -> String {
        let v = share.views == 1 ? "1 view" : "\(share.views) views"
        guard let last = share.lastViewedAt else { return share.views == 0 ? "Not opened yet" : v }
        return "\(v) · last opened \(RelativeTime.string(last, now: now))"
    }

    static func comments(_ share: Share) -> String? {
        guard share.commentCount > 0 else { return share.commentsEnabled ? nil : "Comments off" }
        let c = share.commentCount == 1 ? "1 comment" : "\(share.commentCount) comments"
        return share.unreadComments > 0 ? "\(c) · \(share.unreadComments) new" : c
    }
}
