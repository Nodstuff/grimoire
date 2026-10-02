import Foundation

/// A doc the `[[` popup can offer.
public struct WikiCandidate: Hashable, Sendable, Identifiable {
    public var id: DocID
    public var title: String
    /// "Grimoire › iOS app": where it lives (nil at the top level)
    public var breadcrumb: String?

    public init(id: DocID, title: String, breadcrumb: String? = nil) {
        self.id = id
        self.title = title
        self.breadcrumb = breadcrumb
    }
}

/// `[[` autocomplete: the open query at the caret, fuzzy ranking over doc
/// titles and breadcrumbs, and what to insert.
public enum WikiCompletion {
    /// The text typed after an unclosed `[[` on the caret's line, or nil.
    /// `before` is the block's text up to the caret.
    public static func query(before: String) -> String? {
        guard let open = before.range(of: "[[", options: .backwards) else { return nil }
        let q = before[open.upperBound...]
        if q.contains("]") || q.contains("[") || q.contains("\n") || q.count > 80 { return nil }
        return String(q)
    }

    static func fold(_ s: String) -> String {
        s.folding(options: [.caseInsensitive, .diacriticInsensitive, .widthInsensitive], locale: nil)
    }

    /// Best first. Every word of the query (split on spaces and `/`) has
    /// to match the title or the breadcrumb; title matches outrank
    /// breadcrumb ones, and within the title: exact, prefix, word prefix,
    /// substring, then letters in order. Ties: shorter title, then A–Z.
    public static func rank(_ query: String, in candidates: [WikiCandidate], limit: Int = 8) -> [WikiCandidate] {
        let words = fold(query).split { $0 == " " || $0 == "/" }.map(String.init)
        if words.isEmpty {
            return Array(candidates.sorted { ($0.title.count, $0.title) < ($1.title.count, $1.title) }.prefix(limit))
        }
        let whole = fold(query).trimmingCharacters(in: .whitespaces)
        var scored: [(WikiCandidate, Int)] = []
        for c in candidates {
            let title = fold(c.title)
            let crumb = fold(c.breadcrumb ?? "")
            var total = 0
            var ok = true
            for w in words {
                let t = score(w, in: title)
                let b = score(w, in: crumb) / 4
                let best = max(t, b)
                if best == 0 { ok = false; break }
                total += best
            }
            guard ok else { continue }
            if title == whole { total += 10_000 } else if title.hasPrefix(whole) { total += 2_000 }
            scored.append((c, total))
        }
        return scored.sorted { a, b in
            if a.1 != b.1 { return a.1 > b.1 }
            if a.0.title.count != b.0.title.count { return a.0.title.count < b.0.title.count }
            return a.0.title < b.0.title
        }.prefix(limit).map(\.0)
    }

    /// 0 = no match.
    static func score(_ word: String, in text: String) -> Int {
        guard !text.isEmpty else { return 0 }
        if text == word { return 1000 }
        if text.hasPrefix(word) { return 800 }
        if let r = text.range(of: word) {
            let before = text[..<r.lowerBound].last
            let atWord = before.map { !$0.isLetter && !$0.isNumber } ?? true
            return atWord ? 600 : 400 - min(100, text.distance(from: text.startIndex, to: r.lowerBound))
        }
        // letters in order, rewarded for runs
        var i = text.startIndex
        var runs = 0
        var last: String.Index?
        for ch in word {
            guard let found = text[i...].firstIndex(of: ch) else { return 0 }
            if let last, text.index(after: last) == found { runs += 1 }
            last = found
            i = text.index(after: found)
        }
        return 100 + runs * 10
    }

    /// What goes between the brackets: the title, or `Folder/Title` when
    /// another doc has the same title.
    public static func target(for c: WikiCandidate, among all: [WikiCandidate]) -> String {
        let clash = all.contains { $0.id != c.id && $0.title.caseInsensitiveCompare(c.title) == .orderedSame }
        guard clash, let crumb = c.breadcrumb, let parent = crumb.components(separatedBy: " › ").last else { return c.title }
        return "\(parent)/\(c.title)"
    }
}

/// Creating a doc once, even when the answer gets lost.
public enum NewDoc {
    /// `POST /api/docs` with an idempotency key. A transport failure may
    /// have created it anyway: before trying again, look for a doc with
    /// this title under this parent that wasn't there before (`known`).
    public static func create(
        api: APIClient, title: String, parent: DocID?, requestID: String = UUID().uuidString.lowercased(),
        known: Set<DocID>, attempts: Int = 3, workspaceID: WorkspaceID? = nil
    ) async throws -> DocSummary {
        var lastError: (any Error)?
        for attempt in 0..<attempts {
            if attempt > 0, let tree = try? await api.tree(),
               let made = tree.first(where: { $0.title == title && $0.parentID == parent && !known.contains($0.id) }) {
                return made
            }
            do {
                return try await api.createDoc(title: title, parent: parent, requestID: requestID, workspaceID: workspaceID)
            } catch let e as APIError where !e.isTransient {
                throw e
            } catch {
                lastError = error
            }
        }
        throw lastError ?? APIError.http(status: 0)
    }
}
