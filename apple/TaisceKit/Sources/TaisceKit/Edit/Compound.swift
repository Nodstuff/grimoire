import CryptoKit
import Foundation
import Markdown

/// A text block that holds more than one text element (a line of text then
/// a list, or lists split by blank lines: what agents and imports often
/// write). The surface shows it as ordinary text: each part is a block of
/// its own on screen, the first keeping the server block's id, the others
/// a stable id derived from it. A save folds the parts back into the one
/// server block, so an untouched compound block is never written and an
/// edited one is a single replace; a part moved away from its block (text
/// typed between them) becomes a block of its own.
public enum Compound {
    private static let lock = NSLock()
    nonisolated(unsafe) private static var origins: [BlockID: BlockID] = [:]

    /// The server block a part was split from (nil for any other id).
    public static func origin(of id: BlockID) -> BlockID? { lock.withLock { origins[id] } }

    /// Part `k` (k ≥ 1) of a block: the same id for the same block every time.
    static func partID(_ origin: BlockID, _ k: Int) -> BlockID {
        var b = Array(SHA256.hash(data: Data("taisce.part:\(origin):\(k)".utf8)).prefix(16))
        b[6] = (b[6] & 0x0F) | 0x50
        b[8] = (b[8] & 0x3F) | 0x80
        let id = UUID(uuid: (b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]))
        return id.uuidString.lowercased()
    }

    /// The parts of a block's markdown, when it holds several, at least one
    /// is text the surface edits, and together they mean the same as the
    /// block. Nil otherwise.
    public static func parts(_ md: String) -> [String]? {
        guard !md.contains("```"), !md.contains("~~~"), !md.hasPrefix("---") else { return nil }
        // blank-line separated chunks
        var chunks: [[String]] = [[]]
        for line in md.split(separator: "\n", omittingEmptySubsequences: false) {
            if line.trimmingCharacters(in: .whitespaces).isEmpty {
                if !chunks[chunks.count - 1].isEmpty { chunks.append([]) }
            } else {
                chunks[chunks.count - 1].append(String(line))
            }
        }
        if chunks.last?.isEmpty == true { chunks.removeLast() }
        // a chunk holding several elements (text then a list) splits at each
        var out: [String] = []
        for lines in chunks {
            let text = lines.joined(separator: "\n")
            let kids = Array(Document(parsing: text, options: [.disableSmartOpts]).children)
            if kids.count <= 1 { out.append(text); continue }
            let starts = kids.compactMap { $0.range?.lowerBound.line }
            guard starts.count == kids.count else { return nil }
            for (i, s) in starts.enumerated() {
                let e = i + 1 < starts.count ? starts[i + 1] - 1 : lines.count
                guard s >= 1, e >= s, e <= lines.count else { return nil }
                out.append(lines[(s - 1)..<e].joined(separator: "\n"))
            }
        }
        guard out.count >= 2 else { return nil }
        // worth it when some part is text the surface edits (a table or a
        // callout part stays a source block, shown in place)
        let contents = out.map { EditorBlockContent.parse(markdown: $0) }
        guard contents.contains(where: { !$0.isRaw }) else { return nil }
        let canonical = contents.map(\.markdown)
        guard EditorBlockContent.sameMeaning(md, canonical.joined(separator: "\n\n")) else { return nil }
        return out
    }

    /// Blocks as the surface shows them: a compound text block becomes its parts.
    public static func split(_ blocks: [Block]) -> [Block] {
        var out: [Block] = []
        for b in blocks {
            guard [.paragraph, .heading, .decision].contains(b.blockType), EditorBlockContent.parse(b).isRaw,
                  let ps = parts(b.content) else {
                out.append(b)
                continue
            }
            for (k, p) in ps.enumerated() {
                var part = b
                part.content = p
                if k > 0 {
                    part.id = partID(b.id, k)
                    part.blockType = .paragraph
                    lock.withLock { origins[part.id] = b.id }
                }
                out.append(part)
            }
        }
        return out
    }

    /// Runs of entries that fold together: a part right after its block
    /// (or after another part of it) joins it.
    static func runs<T>(_ items: [T], id: (T) -> BlockID?) -> [[T]] {
        var out: [[T]] = []
        var last: BlockID?
        for item in items {
            let i = id(item)
            if let i, let o = origin(of: i), o == last, !out.isEmpty {
                out[out.count - 1].append(item)
                continue
            }
            out.append([item])
            last = i.map { origin(of: $0) ?? $0 }
        }
        return out
    }

    /// The surface's entries as the server's blocks.
    public static func fold(_ entries: [DocDiff.Entry]) -> [DocDiff.Entry] {
        runs(entries, id: \.id).map { run in
            var e = run[0]
            if run.count > 1 {
                e.content = run.map(\.content).joined(separator: "\n\n")
                // the one server block's level, as before it was split
                e.level = DocDiff.level(e.content)
            }
            return e
        }
    }

    /// The baseline as the server's blocks.
    public static func fold(_ baseline: [DocDiff.BaselineBlock]) -> [DocDiff.BaselineBlock] {
        runs(baseline, id: \.id).map { run in
            var b = run[0]
            if run.count > 1 { b.content = run.map(\.content).joined(separator: "\n\n") }
            return b
        }
    }

    /// A folded baseline (after a save) back in the surface's parts, where
    /// the surface still holds them as saved.
    public static func unfold(_ baseline: [DocDiff.BaselineBlock], entries: [DocDiff.Entry]) -> [DocDiff.BaselineBlock] {
        var byHead: [BlockID: [DocDiff.Entry]] = [:]
        for run in runs(entries, id: \.id) where run.count > 1 {
            if let head = run[0].id { byHead[head] = run }
        }
        var out: [DocDiff.BaselineBlock] = []
        for b in baseline {
            guard let run = byHead[b.id], b.content == run.map(\.content).joined(separator: "\n\n") else {
                out.append(b)
                continue
            }
            for (i, e) in run.enumerated() {
                out.append(DocDiff.BaselineBlock(id: i == 0 ? b.id : (e.id ?? b.id), content: e.content, parent: b.parent, orderKey: b.orderKey))
            }
        }
        return out
    }
}
