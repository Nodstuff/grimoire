import Foundation

public enum OutputStreamKind: Sendable, Hashable { case stdout, stderr }

/// A run of text from one stream, in arrival order.
public struct OutputChunk: Sendable, Hashable {
    public var stream: OutputStreamKind
    public var text: String

    public init(_ stream: OutputStreamKind, _ text: String) {
        self.stream = stream
        self.text = text
    }
}

/// What a run's output looks like on screen: chunks in arrival order,
/// neighbours from the same stream merged.
public struct OutputLog: Sendable, Hashable {
    public static let truncatedMarker = "(output truncated)"
    public private(set) var chunks: [OutputChunk] = []
    public var truncated = false

    public init(_ chunks: [OutputChunk] = []) {
        append(chunks)
    }

    public mutating func append(_ new: [OutputChunk]) {
        for c in new where !c.text.isEmpty {
            if let last = chunks.indices.last, chunks[last].stream == c.stream {
                chunks[last].text += c.text
            } else {
                chunks.append(c)
            }
        }
    }

    /// Everything, both streams, as plain text.
    public var text: String { chunks.map(\.text).joined() }
    public var isEmpty: Bool { chunks.isEmpty }

    /// The last `limit` characters, cut at a line start where possible: a
    /// screen never lays out a megabyte. `dropped` is how much was left off.
    public func tail(_ limit: Int) -> (chunks: [OutputChunk], dropped: Bool) {
        var budget = limit
        var out: [OutputChunk] = []
        for c in chunks.reversed() {
            guard budget > 0 else { return (out.reversed(), true) }
            if c.text.count <= budget {
                out.append(c)
                budget -= c.text.count
            } else {
                var cut = String(c.text.suffix(budget))
                if let nl = cut.firstIndex(of: "\n") { cut = String(cut[cut.index(after: nl)...]) }
                if !cut.isEmpty { out.append(OutputChunk(c.stream, cut)) }
                return (out.reversed(), true)
            }
        }
        return (out.reversed(), false)
    }
}

/// One stream's bytes → display text: ANSI escape sequences (colour, cursor,
/// OSC titles) are removed, and a UTF-8 character or escape split across
/// two reads is carried to the next.
public struct TerminalTextDecoder: Sendable {
    enum State: Sendable { case text, escape, escapeIntermediate, csi, osc, oscEscape }
    var state = State.text
    var carry: [UInt8] = []

    public init() {}

    public mutating func decode(_ bytes: some Sequence<UInt8>, final: Bool = false) -> String {
        var out = carry
        carry = []
        for b in bytes {
            switch state {
            case .text:
                if b == 0x1B { state = .escape } else { out.append(b) }
            case .escape:
                switch b {
                case UInt8(ascii: "["): state = .csi
                case UInt8(ascii: "]"): state = .osc
                case 0x20...0x2F: state = .escapeIntermediate
                default: state = .text // a two-byte sequence (ESC 7, ESC =, …)
                }
            case .escapeIntermediate:
                if !(0x20...0x2F).contains(b) { state = .text }
            case .csi:
                if (0x40...0x7E).contains(b) { state = .text }
            case .osc:
                if b == 0x07 { state = .text } else if b == 0x1B { state = .oscEscape }
            case .oscEscape:
                state = b == UInt8(ascii: "\\") ? .text : .osc
            }
        }
        if !final {
            // hold back an incomplete UTF-8 sequence at the end
            let n = Self.incompleteTail(out)
            if n > 0 {
                carry = Array(out.suffix(n))
                out.removeLast(n)
            }
        }
        return String(decoding: out, as: UTF8.self)
    }

    /// How many trailing bytes start a UTF-8 character that isn't complete.
    static func incompleteTail(_ b: [UInt8]) -> Int {
        let n = b.count
        for back in 1...min(3, max(n, 1)) where n >= back {
            let byte = b[n - back]
            if byte & 0xC0 == 0x80 { continue } // continuation: keep looking
            let need = byte >= 0xF0 ? 4 : byte >= 0xE0 ? 3 : byte >= 0xC0 ? 2 : 1
            return need > back ? back : 0
        }
        return 0
    }

    /// Plain-text helper (tests, one-shot strings).
    public static func strip(_ s: String) -> String {
        var d = TerminalTextDecoder()
        return d.decode(Array(s.utf8), final: true)
    }
}

/// Collects a run's raw output off the main actor: decodes each stream,
/// enforces the byte cap, and hands over what arrived since the last
/// `drain()` as merged chunks (the runner drains every ~50 ms, so a chatty
/// program costs the UI one update per tick, not one per read).
public struct OutputCollector: Sendable {
    public static let defaultCap = 1 << 20

    public let cap: Int
    public private(set) var bytes = 0
    public private(set) var truncated = false
    var stdout = TerminalTextDecoder()
    var stderr = TerminalTextDecoder()
    var pending = OutputLog()

    public init(cap: Int = Self.defaultCap) {
        self.cap = cap
    }

    /// Bytes past the cap are counted as dropped and never decoded.
    public mutating func add(_ data: some Collection<UInt8>, from stream: OutputStreamKind) {
        guard !truncated else { return }
        var slice = Array(data)
        if bytes + slice.count > cap {
            slice = Array(slice.prefix(cap - bytes))
            truncated = true
        }
        bytes += slice.count
        let text = stream == .stdout ? stdout.decode(slice) : stderr.decode(slice)
        pending.append([OutputChunk(stream, text)])
    }

    /// End of output: anything carried (a cut-off character) comes out.
    public mutating func finish() {
        let a = stdout.decode([], final: true)
        let b = stderr.decode([], final: true)
        pending.append([OutputChunk(.stdout, a), OutputChunk(.stderr, b)])
    }

    public mutating func drain() -> [OutputChunk] {
        defer { pending = OutputLog() }
        return pending.chunks
    }
}
