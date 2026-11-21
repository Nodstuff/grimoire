import Foundation
import Observation
import TaisceKit

/// A notification about a session was tapped: the frontmost window shows
/// Agents with that session in view.
struct FleetSessionRequest: Equatable {
    var session: FleetSessionKey
    var serial: Int
}

/// The person's fleet on this device (Agent Fabric): every Claude session
/// their runtimes report and what waits for them, followed live while the
/// app is in front. Commands (Slices 3–5) go through `command`, the agents'
/// conversations through `talk`, settings and pairing through `config`:
/// each takes the stream's events from here. One per app (`AppModel.fleet`);
/// nothing is kept on disk but this device's key and the runtimes' keys,
/// and sign-out forgets the rest (`reset`).
@MainActor @Observable
final class FleetStore {
    enum Availability: Equatable {
        /// the server hasn't answered yet
        case unknown
        case available
        /// the server has no fleet for us: a LOCAL daemon, or one without Agent Fabric (404)
        case unavailable
    }

    @ObservationIgnored weak var app: AppModel?
    /// tests: a fake server
    @ObservationIgnored var serviceOverride: (any FleetService)?
    /// tests: no waiting between connections
    @ObservationIgnored var backoff = Backoff()

    private(set) var fleet = Fleet()
    private(set) var availability: Availability = .unknown
    private(set) var link: FleetLink = .idle
    /// why the fleet isn't loading (nil once it has)
    private(set) var loadError: String?
    /// a tapped notification's session, waiting for a window
    private(set) var request: FleetSessionRequest?
    /// Agents screens in front in an active window
    private(set) var watching = 0
    /// finished turns the stream has sent since the screens last read, per
    /// session, oldest first (a session screen merges them into its feed)
    private(set) var liveTurns: [FleetSessionKey: [FabricTurn]] = [:]
    /// a session screen in front: its notifications are not presented
    private(set) var openSession: FleetSessionKey?

    /// orders, undo and approvals (Slice 3)
    let command = CommandCenter()
    /// agents talking (Slice 5)
    let talk = TalkStore()
    /// guardrails, limits, fabrics, projects, pairing
    let config = FabricConfigStore()

    @ObservationIgnored private var followTask: Task<Void, Never>?
    /// bumped by `reset`: an answer that comes back for an earlier account
    /// or server is dropped
    @ObservationIgnored private var generation = 0

    private var service: (any FleetService)? { serviceOverride ?? app?.api }
    /// the Slice 2–5 routes (a test's fake may have only the fleet's)
    var fabricService: (any FabricService)? {
        if let serviceOverride { return serviceOverride as? any FabricService }
        return app?.api
    }

    init() {
        command.fleet = self
        talk.fleet = self
        config.fleet = self
    }

    /// the fleet has arrived at least once
    var isLoaded: Bool { availability == .available }
    /// Agents is offered (the sidebar, the Go menu, the palette) unless the
    /// server is known to have no fleet: a LOCAL daemon, where it would be
    /// a way in to nothing
    var isOffered: Bool { availability != .unavailable }
    /// the Agents screen is on screen: what a notification would say is already in view
    var isOnScreen: Bool { watching > 0 }

    func reset() {
        stop()
        generation += 1
        fleet = Fleet()
        availability = .unknown
        loadError = nil
        request = nil
        liveTurns = [:]
        command.reset()
        talk.reset()
        config.reset()
    }

    // MARK: following

    /// Follow the fleet's stream. `AppModel.startSync` calls this and
    /// `stopSync` ends it: foreground only, and only while signed in (or on
    /// a server that needs no sign-in), like sync. Asked again while
    /// following, it does nothing. A server with no fleet answers once and
    /// the following ends; so does one that refuses the token.
    func start() {
        guard followTask == nil, let service else { return }
        let follower = FleetFollower(service: service, backoff: backoff)
        followTask = Task { [weak self] in
            for await signal in follower.signals() {
                guard !Task.isCancelled else { break }
                self?.take(signal)
            }
        }
    }

    func stop() {
        followTask?.cancel()
        followTask = nil
        link = .idle
    }

    private func take(_ signal: FleetSignal) {
        switch signal {
        case .change(let change, let rev):
            fleet.apply(change, rev: rev)
            if case .fleet = change {
                availability = .available
                loadError = nil
            }
        case .link(let next):
            link = next
            if next == .unavailable {
                availability = .unavailable
                fleet = Fleet()
                loadError = nil
                followTask = nil
            } else if next == .idle {
                // the follower gave up (the session is over): the next start follows afresh
                followTask = nil
            }
        case .failed(let why):
            // a fleet already showing stays; the indicator says it is stale
            if availability != .available { loadError = Self.describe(why) }
        case .event(let event):
            take(event)
        }
    }

    /// One of the stream's Slice 2–5 events.
    func take(_ event: FabricStreamEvent) {
        switch event {
        case .turn(let turn):
            var list = liveTurns[turn.session] ?? []
            list.removeAll { $0.turnID == turn.turnID }
            list.append(turn)
            liveTurns[turn.session] = Array(list.suffix(50))
        case .attention(let item):
            fleet.raise(item)
        case .attentionCleared(let cleared):
            fleet.clear(attention: cleared.id)
        case .order(let order):
            command.take(order)
        case .settings(let view):
            config.take(view)
        case .project(let project):
            config.take(project)
        case .pairing(let pairing):
            config.take(pairing)
        case .message(let message):
            talk.take(message)
        case .conversation(let conversation):
            talk.take(conversation)
        }
    }

    /// The fleet's open items again (the stream's are the same; this is
    /// for a screen coming back after the app slept).
    func refreshAttention() async {
        guard let api = fabricService, let items = try? await api.attention() else { return }
        fleet.attention = items
    }

    /// A session screen is in front (nil: none).
    func sessionOnScreen(_ key: FleetSessionKey?) { openSession = key }

    /// Replace one session as an answer gave it.
    func replace(_ session: FleetSession) {
        if let i = fleet.sessions.firstIndex(where: { $0.key == session.key }) { fleet.sessions[i] = session }
    }

    /// The fleet again, in the server's order (a pull to refresh, or coming
    /// back to the screen: the list holds still in between). It reads; it
    /// never starts the following (that is the app's lifecycle's).
    func refresh() async {
        guard let service else { return }
        let asked = generation
        do {
            let fresh = try await service.fleet()
            // signed out or another server meanwhile: not this account's answer
            guard asked == generation else { return }
            fleet.refresh(with: fresh)
            availability = .available
            loadError = nil
        } catch APIError.notFound, APIError.notAPIRoute {
            guard asked == generation else { return }
            availability = .unavailable
            fleet = Fleet()
            loadError = nil
        } catch is CancellationError {
        } catch {
            guard asked == generation else { return }
            if availability != .available { loadError = Self.describe(error) }
        }
    }

    // MARK: snooze

    /// No notifications for this session until `option` ends. The server
    /// holds them back; the row stays, marked. Nil, or why it failed.
    func snooze(_ key: FleetSessionKey, _ option: FleetSnooze, now: Date = .now) async -> String? {
        await change { try await $0.snoozeSession(key, until: option.until(now: now)) }
    }

    func unsnooze(_ key: FleetSessionKey) async -> String? {
        await change { try await $0.unsnoozeSession(key) }
    }

    private func change(_ call: (any FleetService) async throws -> FleetSession) async -> String? {
        guard let service else { return "Not connected." }
        let asked = generation
        do {
            let session = try await call(service)
            guard asked == generation else { return nil }
            // the stream says the same a moment later; show it now
            replace(session)
            return nil
        } catch APIError.notFound {
            return "That session is gone."
        } catch {
            return Self.describe(error)
        }
    }

    /// Follow a session (every finished turn notifies) or stop.
    func follow(_ key: FleetSessionKey, _ on: Bool) async -> String? {
        guard let api = fabricService else { return "Not connected." }
        let asked = generation
        do {
            let session = try await api.follow(key, on)
            if asked == generation { replace(session) }
            return nil
        } catch {
            return Self.describe(error)
        }
    }

    var generationNow: Int { generation }

    static func describe(_ error: any Error) -> String {
        if let e = error as? FabricError { return FabricWords.refusal(e) }
        if let e = error as? FabricOrderProblem { return FabricWords.problem(e) }
        if let e = error as? APIError {
            switch e {
            case let .server(m): return m
            case .unauthorized: return "Sign in again to see your agents."
            case let .http(status) where status >= 500: return "The server isn't answering right now."
            default: break
            }
        }
        if (error as? AuthError) != nil { return "Sign in again to see your agents." }
        if (error as? URLError) != nil { return "You're offline." }
        return error.localizedDescription
    }

    // MARK: the screen, and notifications

    /// An Agents screen came to the front of an active window, or left it
    /// (screens count themselves through a `FleetScreenPresence`).
    func screenAppeared() { watching += 1 }
    func screenDisappeared() { watching = max(0, watching - 1) }

    /// A tap on a session's notification.
    func requestSession(_ key: FleetSessionKey) {
        request = FleetSessionRequest(session: key, serial: (request?.serial ?? 0) + 1)
    }

    /// The frontmost window takes it, once.
    func takeRequest() -> FleetSessionKey? {
        defer { request = nil }
        return request?.session
    }
}

/// One Agents screen's entry in `FleetStore.watching`: counted while the
/// screen is in front of an active window, and not otherwise. The screen
/// tells it each time that changes AND when it goes away for good (a
/// sidebar switch or a pop removes the view without any change the view
/// could observe), so a screen that is gone never stays counted.
@MainActor final class FleetScreenPresence {
    private(set) var counted = false

    /// True when this call brought the screen to the front.
    @discardableResult
    func update(inFront: Bool, in store: FleetStore) -> Bool {
        guard inFront != counted else { return false }
        counted = inFront
        if inFront { store.screenAppeared() } else { store.screenDisappeared() }
        return inFront
    }
}
