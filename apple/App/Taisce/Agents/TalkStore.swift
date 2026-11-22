import Foundation
import Observation
import TaisceKit

/// Agents talking to each other (Slice 5): the conversations between the
/// person's sessions, their messages (a body is gone once read), and the
/// hub's items that only the person can settle: a paused pair, a wake
/// offer, an ask waiting on a session that is asleep.
@MainActor @Observable
final class TalkStore {
    @ObservationIgnored weak var fleet: FleetStore?

    private(set) var conversations: [FabricConversation] = []
    /// per conversation, newest first
    private(set) var messages: [String: [FabricMessage]] = [:]
    private(set) var loaded = false
    private(set) var loadError: String?

    private var service: (any FabricService)? { fleet?.fabricService }

    func reset() {
        conversations = []
        messages = [:]
        loaded = false
        loadError = nil
    }

    /// The hub's items: what Talk asks the person to settle.
    var items: [FabricAttentionItem] {
        (fleet?.fleet.attention ?? []).filter { $0.source == .hub || $0.kind == .wakeOffer || $0.kind == .pairPaused || $0.kind == .askWaiting }
    }

    /// The count Talk's entry carries.
    var waiting: Int { items.count }

    /// The fleet's generation when a request went out: an answer that
    /// lands after a sign-out or a server switch (`FleetStore.reset`) is
    /// the previous account's, and is dropped.
    private var generation: Int? { fleet?.generationNow }

    func load() async {
        guard let service else { return }
        let asked = generation
        do {
            let list = try await service.conversations()
            guard generation == asked else { return }
            conversations = list
            loaded = true
            loadError = nil
        } catch {
            guard generation == asked else { return }
            if !loaded { loadError = FleetStore.describe(error) }
        }
    }

    func open(_ id: String) async {
        guard let service else { return }
        let asked = generation
        if let page = try? await service.conversation(id, limit: 100), generation == asked {
            messages[id] = page.messages
            upsert(page.conversation)
        }
    }

    func take(_ m: FabricMessage) {
        var list = messages[m.conversationID] ?? []
        if let i = list.firstIndex(where: { $0.id == m.id }) { list[i] = m } else { list.insert(m, at: 0) }
        messages[m.conversationID] = list
    }

    func take(_ c: FabricConversation) {
        upsert(c)
        conversations.sort { $0.lastAt > $1.lastAt }
    }

    private func upsert(_ c: FabricConversation) {
        if let i = conversations.firstIndex(where: { $0.id == c.id }) { conversations[i] = c } else { conversations.insert(c, at: 0) }
    }

    func conversation(_ id: String) -> FabricConversation? { conversations.first { $0.id == id } }

    // MARK: settling

    /// A paused pair goes on: its count restarts and held messages go.
    func continuePair(_ id: String) async -> String? {
        await settle(id) { try await $0.continueConversation(id) }
    }

    /// Held messages bounce to their sender ("your person stopped this exchange"); the count restarts.
    func stopPair(_ id: String) async -> String? {
        await settle(id) { try await $0.stopConversation(id) }
    }

    /// "Let it wait": the hub's item goes; the mail waits for the session's next turn.
    func dismiss(_ item: FabricAttentionItem) async -> String? {
        guard let service else { return "Not connected." }
        do {
            try await service.dismissAttention(item.id)
            fleet?.take(.attentionCleared(FabricAttentionCleared(id: item.id)))
            return nil
        } catch {
            return FleetStore.describe(error)
        }
    }

    /// "Wake to answer": the person chose to spend the turn. An ask
    /// waiting on a session that has no process is a resume instead.
    func wake(_ item: FabricAttentionItem) async -> String? {
        guard let command = fleet?.command else { return "Not connected." }
        let session = fleet?.fleet.session(item.session)
        let verb: FabricVerb = (session?.state == .asleep || session?.state == .exited) ? .resume : .wake
        if let failure = await command.command(verb, item.session) { return failure }
        // the runtime clears the hub's item; say so here at once
        _ = await dismiss(item)
        return nil
    }

    private func settle(_ id: String, _ call: (any FabricService) async throws -> Void) async -> String? {
        guard let service else { return "Not connected." }
        do {
            try await call(service)
            if let i = conversations.firstIndex(where: { $0.id == id }) { conversations[i].paused = false }
            for item in items where item.conversationID == id && item.kind == .pairPaused {
                fleet?.take(.attentionCleared(FabricAttentionCleared(id: item.id)))
            }
            await open(id)
            return nil
        } catch {
            return FleetStore.describe(error)
        }
    }
}
