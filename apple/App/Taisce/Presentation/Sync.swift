import Foundation
import TaisceKit

/// The small sync chip: green "Saved" when live with nothing queued.
struct SyncBadge: Hashable, Sendable {
    enum Tone: Hashable, Sendable { case saved, busy, offline, idle }

    var tone: Tone
    var text: String

    static func make(status: SyncStatus, pending: Int) -> SyncBadge {
        switch (status, pending) {
        case (.live, 0): SyncBadge(tone: .saved, text: "Saved")
        case (.live, let n): SyncBadge(tone: .busy, text: "Saving · \(n)")
        case (.catchingUp, 0): SyncBadge(tone: .busy, text: "Syncing")
        case (_, 0): SyncBadge(tone: .idle, text: "Offline")
        case (_, let n): SyncBadge(tone: .offline, text: "Offline · \(n) pending")
        }
    }

    static func describe(_ status: SyncStatus) -> String {
        switch status {
        case .idle: "Stopped"
        case .catchingUp: "Catching up"
        case .live: "Live"
        case .waiting(let d): "Reconnecting in \(max(1, Int(d.components.seconds))) s"
        }
    }
}
