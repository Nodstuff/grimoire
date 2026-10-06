import Foundation
import Observation
import TaisceKit
import UserNotifications

/// The three actions on a due alert.
enum DueAlertAction: String, CaseIterable, Sendable {
    case done = "TODO_DONE"
    case snooze = "TODO_SNOOZE_1H"
    case tomorrow = "TODO_TOMORROW_9"

    var title: String {
        switch self {
        case .done: "Done"
        case .snooze: "Snooze 1 hour"
        case .tomorrow: "Tomorrow 09:00"
        }
    }
}

/// `UNUserNotificationCenter` in the planner's terms, so tests can fake it.
@MainActor
protocol DueAlertCenter: AnyObject {
    /// an action tapped on one of our alerts: (action, day, item id)
    var onAction: (@MainActor (DueAlertAction, String, String) async -> Void)? { get set }
    func authorizationStatus() async -> DueAlertStatus
    func requestAuthorization() async
    func registerCategory()
    func pending() async -> [PlannedAlert]
    func add(_ alert: PlannedAlert) async throws
    func removePending(_ identifiers: [String])
}

/// Local due alerts (no APNs): keeps the pending notifications in step with
/// the open to-dos, and answers Done / Snooze / Tomorrow through the outbox,
/// so the actions work offline.
@MainActor @Observable
final class NotificationCoordinator: DueAlertPermission {
    nonisolated static let category = "TODO_DUE"
    /// One reconcile's budget against the notification center.
    nonisolated static let applyLimit: Duration = .seconds(10)

    private(set) var status: DueAlertStatus = .notDetermined

    @ObservationIgnored private let center: any DueAlertCenter
    @ObservationIgnored private let now: @MainActor () -> Date
    @ObservationIgnored private let timeZone: @MainActor () -> TimeZone
    @ObservationIgnored private var cache: Cache?
    @ObservationIgnored private var api: APIClient?
    /// actions answered locally that the cached To-do doc doesn't show yet
    @ObservationIgnored private(set) var overrides: [String: DueAlertOverride] = [:]
    /// actions that arrived before a cache was connected (a cold launch)
    @ObservationIgnored private var queued: [(DueAlertAction, String, String)] = []
    @ObservationIgnored private var followTask: Task<Void, Never>?
    @ObservationIgnored private var lastReconcile: Task<Void, Never>?
    /// what the last reconcile planned from, re-planned after an action
    @ObservationIgnored private var lastInputs: [DueAlertInput] = []
    /// which workspace's list each planned item ("day/item") is on, so an
    /// action writes to the right list (absent = the legacy list)
    @ObservationIgnored private(set) var lists: [String: WorkspaceScope] = [:]
    @ObservationIgnored private var timeZoneObserver: (any NSObjectProtocol)?

    init(
        center: any DueAlertCenter = SystemDueAlertCenter(),
        now: @escaping @MainActor () -> Date = { .now },
        timeZone: @escaping @MainActor () -> TimeZone = { .current }
    ) {
        self.center = center
        self.now = now
        self.timeZone = timeZone
        center.registerCategory()
        center.onAction = { [weak self] action, day, itemID in
            await self?.handle(action, day: day, itemID: itemID)
        }
        timeZoneObserver = NotificationCenter.default.addObserver(
            forName: .NSSystemTimeZoneDidChange, object: nil, queue: .main
        ) { [weak self] _ in
            Task { @MainActor in await self?.reconcile() }
        }
    }

    // MARK: DueAlertPermission

    func refresh() async {
        status = await center.authorizationStatus()
    }

    func requestAuthorization() async {
        await center.requestAuthorization()
        await refresh()
        await reconcile()
    }

    // MARK: wiring

    /// The server's cache and API (for replaying the outbox after an
    /// action). Follows `sync` and reconciles after updates that touch the
    /// To-do doc.
    func connect(cache: Cache, api: APIClient?, sync: SyncEngine?) {
        self.cache = cache
        self.api = api
        overrides = [:]
        followTask?.cancel()
        // the first reconcile comes from AppModel.startSync
        followTask = Task { [weak self] in
            guard let sync else { return }
            for await update in await sync.updates() {
                guard let self, !Task.isCancelled else { return }
                if await self.touchesTodoDoc(update) { await self.reconcile() }
            }
        }
        let pending = queued
        queued = []
        Task {
            for (action, day, itemID) in pending { await handle(action, day: day, itemID: itemID) }
        }
    }

    private func touchesTodoDoc(_ update: SyncUpdate) async -> Bool {
        if update.treeChanged { return true }
        guard let cache, let docs = try? await cache.docs() else { return false }
        return !update.docIDs.isDisjoint(with: Library.todoDocIDs(in: docs))
    }

    // MARK: reconcile

    /// Every workspace's list: sync caches every To-do doc, so this is the
    /// same set `/api/todo/due` with no `workspace` reads, without a network
    /// wait at launch (and it works offline).
    func reconcile() async {
        guard let cache, let all = try? await cache.todos() else { return }
        let tz = timeZone()
        let docs = (try? await cache.docs()) ?? []
        // ADR 0004: alerts for your own lists only, as the server's /api/todo/due
        let records = Self.alerting(all, docs: docs, workspaces: (try? await cache.workspaces()) ?? [])
        lists = Self.lists(records, docs: docs)
        await reconcile(with: records.compactMap { DueAlertInput($0, timeZone: tz) })
    }

    /// The items whose list is yours: in your Unsorted or a workspace you
    /// own. A shared workspace's list (someone else's) is read on its
    /// screen but never alerts here; a workspace this device doesn't know
    /// (an older server) counts as yours.
    nonisolated static func alerting(_ records: [TodoRecord], docs: [DocRecord], workspaces: [Workspace]) -> [TodoRecord] {
        let foreign = Set(workspaces.filter { !$0.isOwn }.map(\.id))
        guard !foreign.isEmpty else { return records }
        let ws = Dictionary(docs.map { ($0.id, $0.workspaceID) }, uniquingKeysWith: { a, _ in a })
        return records.filter { r in
            guard let resolved = ws[r.docID] ?? nil else { return true }
            return !foreign.contains(resolved)
        }
    }

    nonisolated static func lists(_ records: [TodoRecord], docs: [DocRecord]) -> [String: WorkspaceScope] {
        guard docs.contains(where: { $0.workspaceID != nil }) else { return [:] }
        let ws = Dictionary(docs.map { ($0.id, $0.workspaceID) }, uniquingKeysWith: { a, _ in a })
        var out: [String: WorkspaceScope] = [:]
        for r in records {
            guard let resolved = ws[r.docID] else { continue }
            out["\(r.date)/\(DueAlertInput.serverItemID(position: r.position, text: r.text))"] = WorkspaceScope(resolved)
        }
        return out
    }

    /// Make the pending `todo:` requests match the plan for `inputs`:
    /// remove stale ones, add new or changed ones. Runs one at a time.
    func reconcile(with inputs: [DueAlertInput]) async {
        lastInputs = inputs
        let previous = lastReconcile
        let run = Task { @MainActor in
            await previous?.value
            // bounded: a stalled notification center must not wedge every
            // later reconcile behind it
            _ = await withTimeLimit(Self.applyLimit) { @MainActor in await self.apply(inputs) }
        }
        lastReconcile = run
        await run.value
    }

    private func apply(_ inputs: [DueAlertInput]) async {
        await refresh()
        let (adjusted, kept) = DueAlertPlanner.applying(overrides, to: inputs, now: now())
        overrides = kept
        let desired = status == .allowed ? DueAlertPlanner.plan(adjusted, timeZone: timeZone(), now: now()) : []
        let diff = DueAlertPlanner.diff(pending: await center.pending(), desired: desired)
        if !diff.remove.isEmpty { center.removePending(diff.remove) }
        for alert in diff.add {
            try? await center.add(alert)
        }
    }

    // MARK: actions

    /// Queue the write (it survives offline), hold the answer locally until
    /// a sync shows it, try the outbox now, and reconcile.
    func handle(_ action: DueAlertAction, day: String, itemID: String) async {
        guard let cache else {
            queued.append((action, day, itemID))
            return
        }
        let id = "\(day)/\(itemID)"
        let tz = timeZone()
        let clock = TodoClock().in(lists[id])
        do {
            switch action {
            case .done:
                try await cache.enqueueTodoToggle(date: day, itemID: itemID, done: true, clock: clock)
                overrides[id] = .done
            case .snooze:
                // to the minute (what the server stores), from the instant:
                // a wall time is ambiguous in the repeated October hour
                let at = Date(timeIntervalSince1970: (now().timeIntervalSince1970 / 60).rounded(.down) * 60 + 3600)
                try await cache.enqueueDeadline(date: day, itemID: itemID, deadline: Deadline.at(at), clock: clock)
                overrides[id] = .snoozed(at)
            case .tomorrow:
                var cal = Calendar(identifier: .gregorian)
                cal.timeZone = tz
                guard let tomorrow = cal.date(byAdding: .day, value: 1, to: now()) else { return }
                var due = Due.today(now: tomorrow, in: tz)
                due.hour = DueAlertPlanner.allDayHour
                due.minute = 0
                guard let deadline = Deadline.local(due, in: tz), let at = deadline.alertDate(in: tz) else { return }
                try await cache.enqueueDeadline(date: day, itemID: itemID, deadline: deadline, clock: clock)
                overrides[id] = .snoozed(at)
            }
        } catch {
            return
        }
        if let api {
            try? await OutboxReplayer(api: api, cache: cache).replay()
        }
        await reconcile(with: lastInputs)
    }
}

/// The real notification center.
@MainActor
final class SystemDueAlertCenter: DueAlertCenter {
    var onAction: (@MainActor (DueAlertAction, String, String) async -> Void)?

    private let center = UNUserNotificationCenter.current()
    private var delegate: DueAlertDelegate?

    init() {
        let delegate = DueAlertDelegate { [weak self] action, day, itemID in
            await self?.deliver(action, day, itemID)
        }
        self.delegate = delegate
        center.delegate = delegate
    }

    private func deliver(_ action: DueAlertAction, _ day: String, _ itemID: String) async {
        await onAction?(action, day, itemID)
    }

    func authorizationStatus() async -> DueAlertStatus {
        switch await center.notificationSettings().authorizationStatus {
        case .notDetermined: .notDetermined
        case .denied: .denied
        default: .allowed
        }
    }

    func requestAuthorization() async {
        _ = try? await center.requestAuthorization(options: [.alert, .sound])
    }

    func registerCategory() {
        let actions = DueAlertAction.allCases.map { UNNotificationAction(identifier: $0.rawValue, title: $0.title, options: []) }
        center.setNotificationCategories([
            UNNotificationCategory(identifier: NotificationCoordinator.category, actions: actions, intentIdentifiers: [], options: []),
        ])
    }

    func pending() async -> [PlannedAlert] {
        // the async form: a callback closure here would inherit @MainActor
        // and trap if the system calls it off the main queue
        await center.pendingNotificationRequests().compactMap(Self.plannedAlert(from:))
    }

    func add(_ alert: PlannedAlert) async throws {
        try await center.add(Self.request(for: alert))
    }

    func removePending(_ identifiers: [String]) {
        center.removePendingNotificationRequests(withIdentifiers: identifiers)
    }

    nonisolated static func request(for alert: PlannedAlert) -> UNNotificationRequest {
        let content = UNMutableNotificationContent()
        content.title = alert.title
        content.body = alert.body
        content.sound = .default
        content.categoryIdentifier = NotificationCoordinator.category
        content.threadIdentifier = "todo"
        content.userInfo = ["day": alert.day, "itemID": alert.itemID]
        let trigger = UNCalendarNotificationTrigger(dateMatching: alert.components, repeats: false)
        return UNNotificationRequest(identifier: alert.identifier, content: content, trigger: trigger)
    }

    /// A pending request read back in planner terms; nil for one that isn't ours.
    nonisolated static func plannedAlert(from request: UNNotificationRequest) -> PlannedAlert? {
        guard request.identifier.hasPrefix(DueAlertPlanner.identifierPrefix),
              let trigger = request.trigger as? UNCalendarNotificationTrigger
        else { return nil }
        let info = request.content.userInfo
        return PlannedAlert(
            identifier: request.identifier,
            day: info["day"] as? String ?? "",
            itemID: info["itemID"] as? String ?? "",
            title: request.content.title,
            body: request.content.body,
            fireDate: trigger.nextTriggerDate() ?? .distantPast,
            components: trigger.dateComponents
        )
    }
}

/// Shows alerts while the app is open, and passes action taps on.
final class DueAlertDelegate: NSObject, UNUserNotificationCenterDelegate, Sendable {
    private let handler: @Sendable (DueAlertAction, String, String) async -> Void

    init(handler: @escaping @Sendable (DueAlertAction, String, String) async -> Void) {
        self.handler = handler
    }

    func userNotificationCenter(_ center: UNUserNotificationCenter, willPresent notification: UNNotification) async -> UNNotificationPresentationOptions {
        if let doc = ShareCommentPush.docID(from: notification.request.content.userInfo) {
            await MainActor.run { ShareCommentPush.arrived(doc) }
        }
        return [.banner, .list, .sound]
    }

    func userNotificationCenter(_ center: UNUserNotificationCenter, didReceive response: UNNotificationResponse) async {
        let info = response.notification.request.content.userInfo
        if let doc = ShareCommentPush.docID(from: info) {
            await MainActor.run { ShareCommentPush.deliver(doc) }
            return
        }
        guard let action = DueAlertAction(rawValue: response.actionIdentifier),
              let day = info["day"] as? String, let itemID = info["itemID"] as? String
        else { return }
        await handler(action, day, itemID)
    }
}
