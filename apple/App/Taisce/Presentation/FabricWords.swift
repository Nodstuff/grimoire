import Foundation
import TaisceKit

/// What Agent Fabric's codes say in words (ADR 0008 §11's refusals, the
/// Result reasons in runtime.proto, the app's own checks).
enum FabricWords {
    /// A refusal from the hub, or a runtime's reason for an order it would not carry out.
    static func reason(_ code: String) -> String {
        switch code {
        case "replayed", "order_id_reused": "That was already sent."
        case "unknown_device": "This phone isn't paired with that Mac. Pair it in Settings \u{203A} Agents."
        case "bad_signature": "The Mac couldn't verify this phone's signature."
        case "expired": "It expired before your Mac got it."
        case "too_far": "That expiry is too far ahead."
        case "unsafe_text": "A message can't start with ! or /."
        case "too_long": "That's too long to send."
        case "unsupported": "Your Mac's runtime doesn't know how to do that yet."
        case "wrong_runtime": "That order named another Mac."
        case "queue_full": "Too many orders are waiting for that Mac."
        case "held": "Hold all is on for that Mac."
        case "no_session": "That session is gone."
        case "asleep": "It's asleep. Resume it first."
        case "wrong_turn": "It moved on before your reply arrived."
        case "stale_request": "That has already been dealt with."
        case "stale_queue": "The queue changed before that arrived."
        case "stale_settings": "Settings changed on another device. Look again and save once more."
        case "cancelled": "Cancelled."
        case "busy": "Someone is typing in that pane at the Mac."
        case "already_sent": "Too late to take back: it's on its way."
        case "invalid": "Those settings aren't valid."
        case "malformed", "non_canonical": "The order was malformed."
        case "exited": "That session has exited."
        default: code.replacingOccurrences(of: "_", with: " ").capitalizedFirst
        }
    }

    static func refusal(_ e: FabricError) -> String {
        // the server's own words are better for these two: they name the field
        if e.code == "invalid" || e.code == "malformed" { return e.message }
        return reason(e.code)
    }

    static func problem(_ p: FabricOrderProblem) -> String {
        switch p {
        case .unsafeText: "A message can't start with ! or /: that would run a command no guardrail sees."
        case .emptyText: "Write something first."
        case .tooLong: "That's too long to send (16 KB at most)."
        case .unverified(let why): "Not shown: \(why)"
        case .notPaired: "This phone isn't paired with that Mac, so it can't check the request. Pair it in Settings \u{203A} Agents."
        }
    }

    /// An order's state in a few words, for the queue and the audit trail.
    static func state(_ o: FabricOrderView) -> String {
        switch o.state {
        case .pending: "Waiting at the server"
        case .sent: "On its way"
        case .received: "Delivered"
        case .queued: "Queued"
        case .done: "Done"
        case .refused: o.reason.map(reason) ?? "Refused"
        case .failed: o.reason.map(reason) ?? "Failed"
        case .expired: "Expired before delivery"
        case .withdrawn: "Taken back"
        case .unknown: "Unknown"
        }
    }

    /// Why a queue is paused.
    static func queuePaused(_ why: String) -> String {
        switch why {
        case "question": "Paused: it asked you something"
        case "error": "Paused: the turn ended on an error"
        case "denied": "Paused: a guardrail was denied"
        case "interrupted": "Paused: it was stopped"
        case "typed", "mac", "typing": "Paused: someone typed at the Mac"
        case "": ""
        default: "Paused: \(why)"
        }
    }

    static func turnEnd(_ end: TurnEnd) -> String? {
        switch end {
        case .clean, .unknown: nil
        case .question: "Asked you"
        case .error: "Ended on an error"
        case .interrupted: "Stopped"
        case .denied: "A guardrail was denied"
        }
    }

    static func source(_ s: PromptSource) -> String? {
        switch s {
        case .order: "from your phone"
        case .answer: "your answer"
        case .wake: "woken for mail"
        case .mac, .unknown: nil
        }
    }

    /// "Edited 2 files · +6 −2 · 1 commit"
    static func outcome(_ o: GitOutcome) -> String? {
        guard !o.isEmpty else { return nil }
        var parts: [String] = []
        if o.filesChanged > 0 { parts.append(o.filesChanged == 1 ? "1 file" : "\(o.filesChanged) files") }
        if o.linesAdded > 0 || o.linesRemoved > 0 { parts.append("+\(o.linesAdded) \u{2212}\(o.linesRemoved)") }
        if o.commits > 0 { parts.append(o.commits == 1 ? "1 commit" : "\(o.commits) commits") }
        if let n = o.pullRequestNumber { parts.append("PR #\(n)") }
        return parts.isEmpty ? nil : parts.joined(separator: " \u{00B7} ")
    }

    static func messageKind(_ k: FabricMessageKind) -> String {
        switch k {
        case .note: "Note"
        case .ask: "Asks"
        case .reply: "Replies"
        case .bounce: "Bounced"
        case .unknown: "Message"
        }
    }

    /// "14:52" left; "Expired".
    static func countdown(to unix: Int, now: Date) -> String {
        let left = Int(Double(unix) - now.timeIntervalSince1970)
        guard left > 0 else { return "Expired" }
        if left >= 3600 { return "\(left / 3600) h \((left % 3600) / 60) min" }
        return String(format: "%d:%02d", left / 60, left % 60)
    }
}

extension String {
    var capitalizedFirst: String { prefix(1).uppercased() + dropFirst() }
}
