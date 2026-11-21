import UserNotifications

/// Gives a question's notification its own buttons: one per choice the
/// agent offered (up to four), then "Answer…" for words. The server's
/// category for every question is the same (`FABRIC_QUESTION`), and a
/// category's actions are fixed when it is registered, so each question
/// gets a category of its own here, `FABRIC_QUESTION.<request id>`,
/// registered before the notification is shown. Runs only for a push the
/// server marked `mutable-content`; anything else passes through untouched.
///
/// The buttons only carry the choice's id: the app, when one is tapped,
/// fetches the request, verifies the Mac's signature and answers with the
/// label the Mac signed, never with what this push said.
final class NotificationService: UNNotificationServiceExtension {
    static let prefix = "FABRIC_QUESTION."
    /// per-question categories kept (older ones are for questions long answered)
    static let keep = 12

    override func didReceive(_ request: UNNotificationRequest, withContentHandler contentHandler: @escaping (UNNotificationContent) -> Void) {
        let info = request.content.userInfo
        guard let content = request.content.mutableCopy() as? UNMutableNotificationContent,
              info["kind"] as? String == "fabric_attention", info["attention_kind"] as? String == "question",
              let requestID = info["request_id"] as? String, !requestID.isEmpty,
              let choices = info["choices"] as? [[String: Any]], !choices.isEmpty
        else {
            contentHandler(request.content)
            return
        }
        let id = Self.prefix + requestID.lowercased()
        content.categoryIdentifier = id
        // the handler, content and category stay with this one delivery; nothing else touches them
        let delivery = Delivery(content: content, category: Self.category(id, choices), handler: contentHandler)
        Task {
            let center = UNUserNotificationCenter.current()
            var categories = await center.notificationCategories()
            let mine = categories.filter { $0.identifier.hasPrefix(Self.prefix) }
            if mine.count >= Self.keep {
                // nothing says which is oldest; drop all the extension made before
                categories.subtract(mine)
            }
            categories.insert(delivery.category)
            center.setNotificationCategories(categories)
            // reading back waits for the registration to land before the notification shows
            _ = await center.notificationCategories()
            delivery.deliver()
        }
    }

    static func category(_ id: String, _ choices: [[String: Any]]) -> UNNotificationCategory {
        var actions: [UNNotificationAction] = choices.prefix(4).compactMap { c in
            guard let cid = c["id"] as? String, let label = c["label"] as? String, !cid.isEmpty, !label.isEmpty else { return nil }
            return UNNotificationAction(identifier: "fabric.choice." + cid, title: label, options: [.authenticationRequired])
        }
        actions.append(UNTextInputNotificationAction(identifier: "fabric.answer", title: "Answer\u{2026}", options: [.authenticationRequired],
                                                     textInputButtonTitle: "Send", textInputPlaceholder: "Your answer"))
        return UNNotificationCategory(identifier: id, actions: actions, intentIdentifiers: [])
    }

    override func serviceExtensionTimeWillExpire() {}
}

/// One notification's content and the handler that shows it.
private final class Delivery: @unchecked Sendable {
    let content: UNNotificationContent
    let category: UNNotificationCategory
    let handler: (UNNotificationContent) -> Void

    init(content: UNNotificationContent, category: UNNotificationCategory, handler: @escaping (UNNotificationContent) -> Void) {
        self.content = content
        self.category = category
        self.handler = handler
    }

    func deliver() { handler(content) }
}
