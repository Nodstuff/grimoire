import Foundation

/// Whether a Run needs the "Last edited by …" question first. Pure.
///
/// Per block: the newest applied content op (insert or replace) on the
/// block in the doc's ledger (`GET /api/doc/{id}/history`, which carries
/// each op's principal, target, epoch, provenance and, from the server,
/// whether the principal is the caller's) says who wrote the code.
///
/// "You" is the signed-in human, or one of your own agents (the server's
/// `principal_is_yours`, from ADR 0004 `owner_user`, never a name) in a
/// workspace only you can see (Unsorted counts as yours alone). In a shared
/// workspace every agent asks, your own included; other people and their
/// agents always ask. Unknown ownership (an older server) or unknown
/// sharing asks. Yours → run; anyone else → ask, unless this device already
/// approved the doc and nobody but you has changed it since. Unknown
/// (offline, history hidden from you, the block older than the newest 100
/// ops) → ask, remembered the same way.
///
/// The caveat, by design: a prompt-injected edit by your own agent in a
/// private workspace runs without a question (the block's history still
/// says which agent wrote it).
public enum RunTrust {
    public struct Me: Sendable, Hashable {
        public var principalID: String?
        public var name: String?
        /// the doc's workspace has no member but you (nil = unknown)
        public var privateWorkspace: Bool?

        public init(principalID: String?, name: String?, privateWorkspace: Bool? = nil) {
            self.principalID = principalID
            self.name = name
            self.privateWorkspace = privateWorkspace
        }

        /// The signed-in human themself.
        func isHuman(_ e: DocHistoryEntry) -> Bool {
            if let id = principalID, let theirs = e.principalID { return id == theirs }
            // an older server without principal ids: fall back to the name
            // (an agent can't take a person's name)
            if let name, !name.isEmpty, !e.principalName.isEmpty, e.principalKind != "agent" { return name == e.principalName }
            return false
        }

        /// You, or your own agent in a workspace only you can see.
        func matches(_ e: DocHistoryEntry) -> Bool {
            if isHuman(e) { return true }
            return e.principalKind == "agent" && e.principalIsYours == true && privateWorkspace == true
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
    ///
    /// Some of YOUR OWN ops (the signed-in human's; the server writes these
    /// tags only on its own paths and refuses them from callers) carry
    /// someone else's words, so they are not taken at face value:
    /// - a decline's revert (`review:decline:…`) restores an older version
    ///   whose author the ledger doesn't name: unknown (it asks);
    /// - a rename's link rewrite (`rename:…`) that changed nothing outside
    ///   `[[…]]` links: the write before it says who wrote the rest (unknown
    ///   if that write isn't in the history). One that changed anything else
    ///   is an ordinary write of yours.
    /// The same tags on anyone else's op mean nothing: it is their write.
    /// And a write of yours whose content an earlier op by someone else
    /// wrote word for word (a whole-doc save re-inserting an agent's block
    /// under a new id) is theirs; this looks back only as far as the history
    /// goes (the newest 100 ops).
    public static func author(of block: BlockID, history: [DocHistoryEntry]?, me: Me) -> Author {
        guard let history else { return .unknown }
        let writes = history.indices.filter { i in
            let e = history[i]
            return e.applied && e.targetBlock == block && (e.opType == "insert" || e.opType == "replace")
        }
        for (pos, i) in writes.enumerated() {
            let row = history[i]
            if me.isHuman(row) {
                if row.sourceRefs.contains(where: { $0.hasPrefix("review:decline:") }) { return .unknown }
                if row.sourceRefs.contains(where: { $0.hasPrefix("rename:") }) {
                    guard pos + 1 < writes.count else { return .unknown }
                    let prev = history[writes[pos + 1]]
                    if let c = row.content, let p = prev.content, onlyLinksDiffer(c, p) { continue }
                }
            }
            guard me.matches(row) else { return .other(name(row, me: me)) }
            if let content = row.content,
               let earlier = history[(i + 1)...].first(where: { $0.applied && $0.content == content && !me.matches($0) }) {
                return .other(name(earlier, me: me))
            }
            return .me
        }
        return .unknown
    }

    /// `a` and `b` are the same text apart from what's inside `[[…]]`.
    static func onlyLinksDiffer(_ a: String, _ b: String) -> Bool {
        func blank(_ s: String) -> String {
            s.replacingOccurrences(of: #"\[\[[^\]\n]*\]\]"#, with: "[[]]", options: .regularExpression)
        }
        return blank(a) == blank(b)
    }

    /// Who, for the question: "claude:x, Aoife's agent" from the server's
    /// "claude:x (Aoife)"; your own agent says so and why it asks.
    static func name(_ e: DocHistoryEntry, me: Me) -> String {
        guard !e.principalName.isEmpty else { return "someone else" }
        guard e.principalKind == "agent" else { return e.principalName }
        if e.principalIsYours == true {
            return me.privateWorkspace == nil
                ? "\(e.principalName), your agent (couldn't tell whether this workspace is shared)"
                : "\(e.principalName), your agent, in a shared workspace"
        }
        if let open = e.principalName.lastIndex(of: "("), e.principalName.hasSuffix(")"), open > e.principalName.startIndex {
            let label = e.principalName[..<open].trimmingCharacters(in: .whitespaces)
            let owner = e.principalName[e.principalName.index(after: open)..<e.principalName.index(before: e.principalName.endIndex)]
            return "\(label), \(owner)'s agent"
        }
        return e.principalName
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
