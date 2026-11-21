import UserNotifications

/// Gives a question's notification its own buttons: one per choice the
/// agent offered (up to four), then "Answer…", which opens the app. The
/// server's category for every question is the same (`FABRIC_QUESTION`),
/// and a category's actions are fixed when it is registered, so each
/// question gets a category of its own here, `FABRIC_QUESTION.<request id>`,
/// registered before the notification is shown. Runs only for a push the
/// server marked `mutable-content`; anything else passes through untouched.
///
/// The extension cannot check the Mac's signature (it has neither the
/// session nor the trusted keys), so the push's titles are only shown: each
/// button's identifier binds the title it shows (`QuestionActions`), and
/// the app, when one is tapped, fetches the request, verifies it, and
/// answers only when the label the Mac signed for that choice is the title
/// the person saw.
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
            await CategoryRegistry.shared.register(delivery, prefix: Self.prefix, keep: Self.keep)
            delivery.deliver()
        }
    }

    static func category(_ id: String, _ choices: [[String: Any]]) -> UNNotificationCategory {
        var actions: [UNNotificationAction] = choices.prefix(4).compactMap { c in
            guard let cid = c["id"] as? String, let label = c["label"] as? String, !cid.isEmpty, !label.isEmpty else { return nil }
            return UNNotificationAction(identifier: QuestionActions.identifier(choiceID: cid, title: label), title: label, options: [.authenticationRequired])
        }
        // words go from the app, after it has shown the question the Mac signed
        actions.append(UNNotificationAction(identifier: QuestionActions.answer, title: "Answer\u{2026}", options: [.foreground]))
        return UNNotificationCategory(identifier: id, actions: actions, intentIdentifiers: [])
    }

    override func serviceExtensionTimeWillExpire() {}
}

/// The notification center's categories are one set, read and written whole:
/// two questions arriving together must not each read the set, add their own
/// and write it back over the other's. Registrations here run one at a time.
actor CategoryRegistry {
    static let shared = CategoryRegistry()
    private var tail: Task<Void, Never>?

    func register(_ delivery: Delivery, prefix: String, keep: Int) async {
        let previous = tail
        // set before any await: the next caller chains behind this one
        let mine = Task {
            await previous?.value
            let center = UNUserNotificationCenter.current()
            var categories = await center.notificationCategories()
            let made = categories.filter { $0.identifier.hasPrefix(prefix) }
            if made.count >= keep {
                // nothing says which is oldest; drop all the extension made before
                categories.subtract(made)
            }
            categories.insert(delivery.category)
            center.setNotificationCategories(categories)
            // reading back waits for the registration to land before the notification shows
            _ = await center.notificationCategories()
        }
        tail = mine
        await mine.value
    }
}

/// One notification's content and the handler that shows it.
final class Delivery: @unchecked Sendable {
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
