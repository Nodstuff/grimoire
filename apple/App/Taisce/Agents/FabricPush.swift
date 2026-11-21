import Foundation
import TaisceKit
import UserNotifications

/// Agent Fabric's notifications beyond a session's state (ADR 0008 §10):
/// `fabric_attention` (an approval, a question, a wake offer, a paused
/// pair, an ask waiting), `fabric_order` (an order refused or failed) and
/// the finished-turn summary. Each kind has a category with its actions.
///
/// A push is a pointer, never the thing acted on: every action first reads
/// the item from the server and, for a runtime's, verifies its signature
/// against the Mac's own key before signing anything.
///
/// Approving is never a notification action: it must show the verified
/// command and ask for Face ID, so "Review" opens the approval sheet.
/// Denying, answering and the hub's choices work from the notification
/// (each needs the phone unlocked). A question's own choices become
/// buttons in `TaisceNotifications` (the service extension), which
/// registers a category per question; without it (a push the server did
/// not mark mutable) "Answer…" still takes words.
enum FabricPush {
    static let attentionKind = "fabric_attention"
    static let orderKind = "fabric_order"

    enum Category {
        static let session = "FABRIC_SESSION"
        static let finished = "FABRIC_FINISHED"
        static let approval = "FABRIC_APPROVAL"
        static let question = "FABRIC_QUESTION"
        static let wake = "FABRIC_WAKE"
        static let pair = "FABRIC_PAIR"
        static let ask = "FABRIC_ASK"
        static let order = "FABRIC_ORDER"
        /// the extension's per-question categories: `FABRIC_QUESTION.<request id>`
        static let questionPrefix = "FABRIC_QUESTION."
    }

    enum Action {
        static let review = "fabric.review"
        static let deny = "fabric.deny"
        static let answer = "fabric.answer"
        /// `fabric.choice.<choice id>`
        static let choicePrefix = "fabric.choice."
        static let wake = "fabric.wake"
        static let wait = "fabric.wait"
        static let resume = "fabric.resume"
        static let pairContinue = "fabric.continue"
        static let pairStop = "fabric.stop"
    }

    /// Every fabric category, for `setNotificationCategories` (beside To-dos').
    static var categories: [UNNotificationCategory] {
        let unlocked: UNNotificationActionOptions = [.authenticationRequired]
        let answer = UNTextInputNotificationAction(identifier: Action.answer, title: "Answer\u{2026}", options: unlocked,
                                                   icon: UNNotificationActionIcon(systemImageName: "text.bubble"),
                                                   textInputButtonTitle: "Send", textInputPlaceholder: "Your answer")
        let review = UNNotificationAction(identifier: Action.review, title: "Review", options: [.foreground],
                                          icon: UNNotificationActionIcon(systemImageName: "checkmark.shield"))
        let deny = UNNotificationAction(identifier: Action.deny, title: "Deny", options: unlocked.union(.destructive),
                                        icon: UNNotificationActionIcon(systemImageName: "hand.raised"))
        let wake = UNNotificationAction(identifier: Action.wake, title: "Wake to answer", options: unlocked)
        let resume = UNNotificationAction(identifier: Action.resume, title: "Resume to answer", options: unlocked)
        let wait = UNNotificationAction(identifier: Action.wait, title: "Let it wait", options: [])
        let goOn = UNNotificationAction(identifier: Action.pairContinue, title: "Let them continue", options: unlocked)
        let stop = UNNotificationAction(identifier: Action.pairStop, title: "Stop them", options: unlocked.union(.destructive))
        return [
            UNNotificationCategory(identifier: Category.session, actions: [], intentIdentifiers: []),
            UNNotificationCategory(identifier: Category.finished, actions: [], intentIdentifiers: []),
            UNNotificationCategory(identifier: Category.order, actions: [], intentIdentifiers: []),
            UNNotificationCategory(identifier: Category.approval, actions: [review, deny], intentIdentifiers: []),
            UNNotificationCategory(identifier: Category.question, actions: [answer], intentIdentifiers: []),
            UNNotificationCategory(identifier: Category.wake, actions: [wake, wait], intentIdentifiers: []),
            UNNotificationCategory(identifier: Category.ask, actions: [resume, wait], intentIdentifiers: []),
            UNNotificationCategory(identifier: Category.pair, actions: [goOn, stop], intentIdentifiers: []),
        ]
    }

    /// What a fabric push points at.
    struct Pointer: Equatable, Sendable {
        var kind: String
        var session: FleetSessionKey?
        /// the attention item (`request_id`) or the order (`order_id`)
        var itemID: String?
        var attentionKind: FabricAttentionKind?
    }

    static func pointer(from info: [AnyHashable: Any]) -> Pointer? {
        guard let kind = info["kind"] as? String, kind == attentionKind || kind == orderKind || kind == FabricSessionPush.kind else { return nil }
        var session: FleetSessionKey?
        if let s = info["session_id"] as? String, let r = info["runtime_id"] as? String, !s.isEmpty, !r.isEmpty {
            session = FleetSessionKey(runtimeID: r.lowercased(), id: s.lowercased())
        }
        let item = (info["request_id"] as? String) ?? (info["order_id"] as? String) ?? (info["item_id"] as? String)
        let ak = (info["attention_kind"] as? String).map { FabricAttentionKind(rawValue: $0) ?? .unknown }
        return Pointer(kind: kind, session: session, itemID: item?.lowercased(), attentionKind: ak)
    }

    /// The app's model (the delegate owns it).
    @MainActor static weak var model: AppModel?

    /// A tap on the notification itself: show what it is about.
    @MainActor static func open(_ p: Pointer) {
        guard let model else { return }
        model.fleet.requestOpen(p)
    }

    /// An action button: carried out where the notification was, the app
    /// woken in the background when it was not running. Nil, or what went
    /// wrong (shown as a local notification: there is no screen).
    @MainActor static func act(_ p: Pointer, action: String, text: String?) async -> String? {
        guard let model else { return "Taisce isn't ready." }
        if action == Action.review {
            model.fleet.requestOpen(p)
            return nil
        }
        await model.connectIfNeeded()
        let fleet = model.fleet
        if fleet.fleet.runtimes.isEmpty { await fleet.refresh() }
        await fleet.refreshAttention()
        guard let id = p.itemID, let item = fleet.fleet.attention.first(where: { $0.id == id }) else {
            return "That has already been dealt with."
        }
        switch action {
        case Action.deny, Action.answer:
            switch fleet.command.verify(item) {
            case .failure(let problem):
                return FabricWords.problem(problem)
            case .success(let v):
                if action == Action.deny { return await fleet.command.deny(v) }
                return await fleet.command.answer(v, words: text ?? "")
            }
        case let a where a.hasPrefix(Action.choicePrefix):
            let choiceID = String(a.dropFirst(Action.choicePrefix.count))
            switch fleet.command.verify(item) {
            case .failure(let problem): return FabricWords.problem(problem)
            case .success(let v):
                // only a choice the Mac signed
                guard let choice = v.request.choices.first(where: { $0.id == choiceID }) else { return "That choice isn't one it offered." }
                return await fleet.command.answer(v, choice: choice)
            }
        case Action.wake, Action.resume:
            return await fleet.talk.wake(item)
        case Action.wait:
            return await fleet.talk.dismiss(item)
        case Action.pairContinue:
            return await fleet.talk.continuePair(item.conversationID ?? "")
        case Action.pairStop:
            return await fleet.talk.stopPair(item.conversationID ?? "")
        default:
            return nil
        }
    }

    /// Tell the person an action from a notification did not go through.
    static func report(_ failure: String, about title: String) async {
        let content = UNMutableNotificationContent()
        content.title = title
        content.body = failure
        content.threadIdentifier = "fabric:failures"
        try? await UNUserNotificationCenter.current().add(UNNotificationRequest(identifier: UUID().uuidString, content: content, trigger: nil))
    }

    /// Shown while the app is open: not when it is about the session on screen.
    @MainActor static func presentation(_ p: Pointer) -> UNNotificationPresentationOptions {
        guard let model else { return [.banner, .list, .sound] }
        if let s = p.session, model.fleet.openSession == s { return [] }
        return [.banner, .list, .sound]
    }
}
