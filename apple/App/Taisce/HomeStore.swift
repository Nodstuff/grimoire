import Foundation
import Observation
import TaisceKit
import UIKit

/// What Today shows beside to-dos and pins: the review queue (also the
/// sidebar's Review count and the Today tab's badge) and the signed-in
/// person's name (the book in progress is `BookStore`'s). Network only; nothing is cached.
@MainActor @Observable
final class HomeStore {
    @ObservationIgnored weak var app: AppModel?

    /// open changes waiting for a person; nil until the server has answered
    private(set) var review: [ReviewQueueItem]?
    /// the last queue load failed (the old list, if any, stays)
    private(set) var reviewError: String?
    /// annotation ids being resolved (their buttons wait)
    private(set) var resolving: Set<String> = []
    /// the person's name (`/api/profile`), for the avatar and Settings
    private(set) var profileName: String?

    var summary: ReviewQueueSummary { ReviewQueueSummary(review ?? []) }

    func reset() {
        review = nil
        reviewError = nil
        resolving = []
        profileName = nil
    }

    func loadReview() async {
        guard let api = app?.api else { return }
        do {
            review = try await api.reviewQueue()
            reviewError = nil
        } catch is CancellationError {
        } catch {
            reviewError = error.localizedDescription
        }
    }

    /// Keep / revert / apply / discard. Nil, or why it failed.
    func resolve(_ item: ReviewQueueItem, _ decision: ReviewQueueDecision) async -> String? {
        guard let api = app?.api else { return "Not connected." }
        resolving.insert(item.id)
        defer { resolving.remove(item.id) }
        do {
            try await api.resolveQueued(item.id, decision)
        } catch {
            return ReviewCopy.failure(error)
        }
        review?.removeAll { $0.id == item.id }
        // the doc changed (a revert, an applied park): bring it in now
        try? await app?.sync?.catchUp()
        await loadReview()
        return nil
    }

    func loadProfile() async {
        guard profileName == nil, let api = app?.api else { return }
        if let p = try? await api.profile(), !p.name.isEmpty { profileName = p.name }
    }
}
