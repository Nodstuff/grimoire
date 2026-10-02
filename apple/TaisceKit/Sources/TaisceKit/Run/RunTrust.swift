import Foundation

/// Whether a Run needs the "Last edited by …" question first. Pure.
///
/// Per block: the newest applied content op (insert or replace) on the
/// block in the doc's ledger (`GET /api/doc/{id}/history`, which carries
/// each op's principal, target and epoch) says who wrote the code. Yours →
/// run. Anyone else (an agent such as `claude:…`, another person) → ask,
/// unless this device already approved the doc and nobody but you has
/// changed it since. Unknown (offline, history hidden from you, the block
/// older than the newest 100 ops) → ask, and the answer is remembered the
/// same way.
public enum RunTrust {
    public struct Me: Sendable, Hashable {
        public var principalID: String?
        public var name: String?

        public init(principalID: String?, name: String?) {
            self.principalID = principalID
            self.name = name
        }

        func matches(_ e: DocHistoryEntry) -> Bool {
            if let id = principalID, let theirs = e.principalID { return id == theirs }
            // an older server without principal ids: fall back to the name
            if let name, !name.isEmpty, !e.principalName.isEmpty { return name == e.principalName }
            return false
        }
    }

    /// This device's yes for one doc: the doc's epoch when it was given,
    /// and the epoch of the newest change by someone else at that time.
    public struct Approval: Codable, Sendable, Hashable {
        public var docEpoch: Int
        public var othersEpoch: Int?

        public init(docEpoch: Int, othersEpoch: Int?) {
            self.docEpoch = docEpoch
            self.othersEpoch = othersEpoch
        }
    }

    public enum Author: Sendable, Hashable {
        case me
        case other(String)
        /// no ledger row for the block (offline, hidden, or too old)
        case unknown
    }

    public enum Decision: Sendable, Hashable {
        case run
        /// show the code with "Last edited by <name>. Run it on this Mac?"
        case ask(lastEditedBy: String)
    }

    /// Who last wrote the block's content, from the ledger (newest first).
    public static func author(of block: BlockID, history: [DocHistoryEntry]?, me: Me) -> Author {
        guard let history else { return .unknown }
        guard let row = history.first(where: { $0.applied && $0.targetBlock == block && ($0.opType == "insert" || $0.opType == "replace") }) else {
            return .unknown
        }
        return me.matches(row) ? .me : .other(row.principalName.isEmpty ? "someone else" : row.principalName)
    }

    /// The epoch of the newest applied op in the doc by anyone but `me`
    /// (nil: none in the history, or no history).
    public static func othersEpoch(_ history: [DocHistoryEntry]?, me: Me) -> Int? {
        history?.first(where: { $0.applied && !me.matches($0) })?.epoch
    }

    /// - practiceEditedByMe: the code to run is text typed on this device
    ///   ("Edit to try"), so it is yours whoever wrote the doc's version.
    /// - docEpoch: the doc's current epoch (cache), to judge an approval
    ///   offline: nothing may have changed since it was given.
    public static func decide(
        block: BlockID, history: [DocHistoryEntry]?, me: Me,
        practiceEditedByMe: Bool, approval: Approval?, docEpoch: Int
    ) -> Decision {
        if practiceEditedByMe { return .run }
        let who = author(of: block, history: history, me: me)
        if who == .me { return .run }
        if let approval, stillValid(approval, history: history, me: me, docEpoch: docEpoch) { return .run }
        switch who {
        case .me: return .run
        case .other(let name): return .ask(lastEditedBy: name)
        case .unknown: return .ask(lastEditedBy: history == nil ? "someone (couldn't check: offline)" : "someone else")
        }
    }

    /// The approval to store after a yes.
    public static func approval(history: [DocHistoryEntry]?, me: Me, docEpoch: Int) -> Approval {
        Approval(docEpoch: docEpoch, othersEpoch: othersEpoch(history, me: me))
    }

    /// An approval holds until someone other than you changes the doc:
    /// with the ledger, the newest change by others must be no newer than
    /// at approval; without it, the doc must not have moved at all.
    public static func stillValid(_ a: Approval, history: [DocHistoryEntry]?, me: Me, docEpoch: Int) -> Bool {
        guard let history else { return docEpoch == a.docEpoch }
        guard let now = othersEpoch(history, me: me) else { return true }
        // others' newest change predates the approval, or is the one approved
        if let then = a.othersEpoch { return now <= then }
        return now <= a.docEpoch
    }
}

/// Approvals per doc, kept in UserDefaults on this device (per server).
public struct RunApprovals: @unchecked Sendable {
    let defaults: UserDefaults
    /// the UserDefaults key (forgotten on sign-out with the person's settings)
    public let key: String

    public init(defaults: UserDefaults = .standard, server: String) {
        self.defaults = defaults
        let url = URL(string: server)
        key = "run.approvals-\(url?.host() ?? "server")-\(url?.port ?? 0)"
    }

    public func approval(for doc: DocID) -> RunTrust.Approval? {
        all()[doc]
    }

    public func approve(_ doc: DocID, _ a: RunTrust.Approval) {
        var m = all()
        m[doc] = a
        if let data = try? JSONEncoder().encode(m) { defaults.set(data, forKey: key) }
    }

    public func forget() {
        defaults.removeObject(forKey: key)
    }

    func all() -> [DocID: RunTrust.Approval] {
        guard let data = defaults.data(forKey: key) else { return [:] }
        return (try? JSONDecoder().decode([DocID: RunTrust.Approval].self, from: data)) ?? [:]
    }
}
