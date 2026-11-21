import CryptoKit
import Foundation

/// A question notification's choice buttons, shared by the app and the
/// notification service extension. A button's title comes from the push
/// (the server's words), so its identifier binds the title it shows:
/// `fabric.choice.<tag>.<choice id>`, the tag the first 16 hex characters
/// of SHA-256 over the title. The app answers only with the label the Mac
/// signed for that choice, and only when that label has the same tag: a
/// server that shows "No, stop" over the Mac's "Yes" gets nothing signed.
enum QuestionActions {
    static let choicePrefix = "fabric.choice."
    /// "Answer…": opens the app on the question (its text verified there first)
    static let answer = "fabric.answer"

    static func tag(_ title: String) -> String {
        SHA256.hash(data: Data(title.utf8)).prefix(8).map { String(format: "%02x", $0) }.joined()
    }

    static func identifier(choiceID: String, title: String) -> String {
        choicePrefix + tag(title) + "." + choiceID
    }

    /// The tag and the choice id a button's identifier carries.
    static func parse(_ identifier: String) -> (tag: String, choiceID: String)? {
        guard identifier.hasPrefix(choicePrefix) else { return nil }
        let rest = identifier.dropFirst(choicePrefix.count)
        guard let dot = rest.firstIndex(of: "."), rest.distance(from: rest.startIndex, to: dot) == 16 else { return nil }
        let id = rest[rest.index(after: dot)...]
        return id.isEmpty ? nil : (String(rest[..<dot]), String(id))
    }
}
