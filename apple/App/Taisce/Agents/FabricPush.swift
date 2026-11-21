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
/// Answering in words is not one either: the notification shows the
/// server's text, so "Answer…" opens the app on the question the Mac
/// signed. Denying, a question's choices and the hub's choices work from
/// the notification (each needs the phone unlocked). A question's choices
/// become buttons in `TaisceNotifications` (the service extension), which
/// registers a category per question; each button is bound to the title
/// it shows (`QuestionActions`) and answers only if that is the label the
/// Mac signed.
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
        static let answer = QuestionActions.answer
        /// `fabric.choice.<title tag>.<choice id>` (`QuestionActions`)
        static let choicePrefix = QuestionActions.choicePrefix
        static let wake = "fabric.wake"
        static let wait = "fabric.wait"
        static let resume = "fabric.resume"
        static let pairContinue = "fabric.continue"
        static let pairStop = "fabric.stop"
    }

    /// Every fabric category, for `setNotificationCategories` (beside To-dos').
    static var categories: [UNNotificationCategory] {
        let unlocked: UNNotificationActionOptions = [.authenticationRequired]
        let answer = UNNotificationAction(identifier: Action.answer, title: "Answer\u{2026}", options: [.foreground],
                                          icon: UNNotificationActionIcon(systemImageName: "text.bubble"))
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
        if action == Action.review || action == Action.answer {
            model.fleet.requestOpen(p)
            return nil
        }
        await model.connectIfNeeded()
        return await perform(p, action: action, in: model.fleet)
    }

    /// Whether a tap on `p` opens the approval sheet: only for an item its
    /// Mac signed as an approval, whatever the push calls it. An item not
    /// in the fleet yet (a cold launch) goes by the push's word, because the
    /// sheet verifies before it shows anything and offers nothing to act on
    /// unless the Mac signed an approval.
    @MainActor static func opensApproval(_ p: Pointer, in fleet: FleetStore) -> Bool {
        guard let id = p.itemID else { return false }
        if let item = fleet.fleet.attention.first(where: { $0.id == id }) {
            if let s = p.session, item.session != s { return false }
            return fleet.command.verifiedKind(item) == .approval
        }
        return p.attentionKind == .approval
    }

    /// An action carried out against what the server has now, for `fleet`.
    @MainActor static func perform(_ p: Pointer, action: String, in fleet: FleetStore) async -> String? {
        if action == Action.review || action == Action.answer {
            // both show what the Mac signed before anything is signed
            fleet.requestOpen(p)
            return nil
        }
        if fleet.fleet.runtimes.isEmpty { await fleet.refresh() }
        await fleet.refreshAttention()
        guard let id = p.itemID, let item = fleet.fleet.attention.first(where: { $0.id == id }) else {
            return "That has already been dealt with."
        }
        // the push's pointer, the server's item and (below) the signed request must agree
        if let s = p.session, item.session != s { return "That notification doesn't match what your Mac has waiting." }
        // a signed answer or denial needs the push to name the session as well as the item
        if action == Action.deny || action.hasPrefix(Action.choicePrefix), p.session == nil {
            fleet.requestOpen(p)
            return "That notification doesn't say which session it is about. Nothing was sent; it's open in Taisce."
        }
        switch action {
        case Action.deny:
            switch fleet.command.verify(item) {
            case .failure(let problem): return FabricWords.problem(problem)
            case .success(let v): return await fleet.command.deny(v)
            }
        case let a where a.hasPrefix(Action.choicePrefix):
            guard let parsed = QuestionActions.parse(a) else { return "That choice isn't one it offered." }
            let (tag, choiceID) = parsed
            switch fleet.command.verify(item) {
            case .failure(let problem): return FabricWords.problem(problem)
            case .success(let v):
                // only a choice the Mac signed, and only if its label is what the button said
                guard let choice = v.request.choices.first(where: { $0.id == choiceID }) else { return "That choice isn't one it offered." }
                guard QuestionActions.tag(choice.label) == tag else {
                    fleet.requestOpen(p)
                    return "The notification's button didn't say what your Mac asked. Nothing was sent; it's open in Taisce."
                }
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
