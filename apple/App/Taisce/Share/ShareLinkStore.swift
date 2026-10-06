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
    @ObservationIgnored var snapshotOverride: ((DocID, ShareSnapshot.Theme, Bool) async throws -> ShareSnapshotResult)?

    private(set) var byDoc: [DocID: [Share]] = [:]
    private(set) var all: [Share] = []
    private(set) var allLoaded = false
    /// what each doc's last publish left out or couldn't draw
    private(set) var problems: [DocID: [String]] = [:]
    /// each doc's last publish carried edits still in the outbox
    private(set) var unsentEdits: [DocID: Bool] = [:]

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
        problems = [:]
        unsentEdits = [:]
    }

    /// The sheet opening afresh: an earlier publish's notes no longer apply.
    func clearNotes(for doc: DocID) {
        problems[doc] = nil
        unsentEdits[doc] = nil
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

    /// Build, then (unless cancelled meanwhile: the sheet closed) publish.
    /// The expiry is resolved after the build, just before the POST, so a
    /// slow build can't send a custom date that has come too close.
    func create(doc: DocID, expiry: ShareExpiry, custom: Date?, commentsEnabled: Bool, theme: ShareSnapshot.Theme, fetchImages: Bool) async throws -> Share {
        let service = try service
        let built = try await snapshot(doc, theme, fetchImages)
        note(built, for: doc)
        try Task.checkCancellation()
        let expiresAt = try expiry.resolve(custom: custom)
        let share = try await service.createShare(docID: doc, snapshot: built.snapshot, expiresAt: expiresAt, commentsEnabled: commentsEnabled)
        apply(share)
        return share
    }

    /// Update link: the doc as it is now, at the same URL.
    func republish(_ share: Share, theme: ShareSnapshot.Theme, fetchImages: Bool) async throws {
        let service = try service
        let built = try await snapshot(share.docID, theme, fetchImages)
        note(built, for: share.docID)
        try Task.checkCancellation()
        apply(try await service.updateShare(share.id, snapshot: built.snapshot, expiresAt: .keep, commentsEnabled: nil))
    }

    private func note(_ built: ShareSnapshotResult, for doc: DocID) {
        problems[doc] = built.problems.isEmpty ? nil : built.problems
        unsentEdits[doc] = built.includesUnsentEdits
    }

    func setComments(_ share: Share, enabled: Bool) async throws {
        apply(try await service.updateShare(share.id, snapshot: nil, expiresAt: .keep, commentsEnabled: enabled))
    }

    /// A new expiry, for a live or an expired link (the server refuses a revoked one).
    func setExpiry(_ share: Share, expiry: ShareExpiry, custom: Date?) async throws {
        let date = try expiry.resolve(custom: custom)
        apply(try await service.updateShare(share.id, snapshot: nil, expiresAt: .set(date), commentsEnabled: nil))
    }

    func revoke(_ share: Share) async throws {
        try await service.revokeShare(share.id)
        var s = share
        s.revokedAt = .now
        apply(s)
    }

    // MARK: comments

    /// The owner opening a link's comments: fetch them, then mark them
    /// read (`POST …/comments/read`). The badge clears only when the
    /// server took the mark; a failed mark leaves it, and the comments show.
    func comments(for share: Share) async throws -> [ShareComment] {
        let service = try service
        let list = try await service.shareComments(share.id)
        var s = share
        s.commentCount = list.count
        if share.unreadComments > 0 {
            if (try? await service.markShareCommentsRead(share.id)) != nil { s.unreadComments = 0 }
        }
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

    /// Deleting a comment takes its replies too: the counts come back from the server.
    func delete(_ comment: ShareComment, on share: Share) async throws {
        try await service.deleteShareComment(share.id, commentID: comment.id)
        try await load(doc: share.docID)
    }

    /// A comment push in the foreground: that doc's badges, and the list's.
    func commentArrived(on doc: DocID) async {
        try? await load(doc: doc)
        if allLoaded { try? await loadAll() }
    }

    // MARK: plumbing

    private func snapshot(_ doc: DocID, _ theme: ShareSnapshot.Theme, _ fetchImages: Bool) async throws -> ShareSnapshotResult {
        if let snapshotOverride { return try await snapshotOverride(doc, theme, fetchImages) }
        guard let app else { throw ShareLinkError.notAvailable }
        return try await app.shareSnapshot(doc, theme: theme, fetchImages: fetchImages, purpose: .link)
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

/// What to tell a person when a share call fails: the server's own words
/// (403 not yours to share, 413 too large, 429 a per-user cap), else a
/// plain sentence rather than an error type's name.
enum ShareErrorText {
    static func message(_ error: any Error) -> String {
        switch error {
        case let e as ShareAPIError: return e.localizedDescription
        case let e as ShareLinkError: return e.localizedDescription
        case let e as ShareExpiryTooSoon: return e.localizedDescription
        case let e as APIError:
            switch e {
            case let .server(m), let .notFound(m): return m
            case .unauthorized: return "Your sign-in has expired. Sign in again in Settings."
            case let .http(status): return "The server couldn't do that just now (HTTP \(status)). Try again."
            case .notAPIRoute: return "This server doesn't have share links."
            case .decoding: return "The server's answer didn't make sense to this version of the app."
            case let .badURL(u): return "Bad server address: \(u)"
            }
        default: return error.localizedDescription
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
