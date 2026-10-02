import Foundation

public struct SSEEvent: Sendable, Hashable {
    public var id: String?
    public var event: String
    public var data: String
}

public enum SSEOutput: Sendable, Hashable {
    case event(SSEEvent)
    /// `retry:` — the reconnection delay the server asks for, in milliseconds
    case retry(Int)
    /// `: ping` heartbeats and other comment lines
    case comment(String)
}

/// Incremental `text/event-stream` parser (WHATWG HTML §9.2.6). Feed it bytes
/// in whatever chunks the network delivers; it handles LF, CRLF and lone CR
/// line endings split across chunks, a leading BOM, multi-line `data`, and
/// `id` persistence. Hand-rolled because `AsyncBytes.lines` drops the blank
/// lines that delimit SSE events.
public struct SSEParser: Sendable {
    /// The last `id:` seen; persists across events, as the spec requires.
    public private(set) var lastEventID: String?

    private var line: [UInt8] = []
    private var sawCR = false
    private var started = false
    private var data = ""
    private var eventType = ""
    private var hasData = false

    public init(lastEventID: String? = nil) {
        self.lastEventID = lastEventID
    }

    public mutating func feed(_ bytes: some Sequence<UInt8>) -> [SSEOutput] {
        var out: [SSEOutput] = []
        for b in bytes { feed(byte: b, into: &out) }
        return out
    }

    public mutating func feed(byte b: UInt8, into out: inout [SSEOutput]) {
        if sawCR {
            sawCR = false
            if b == 0x0A { return } // the LF of a CRLF
        }
        switch b {
        case 0x0A:
            endLine(into: &out)
        case 0x0D:
            sawCR = true
            endLine(into: &out)
        default:
            line.append(b)
        }
    }

    private mutating func endLine(into out: inout [SSEOutput]) {
        var text = String(decoding: line, as: UTF8.self)
        line.removeAll(keepingCapacity: true)
        if !started {
            started = true
            if text.hasPrefix("\u{FEFF}") { text.removeFirst() }
        }
        if text.isEmpty {
            dispatch(into: &out)
            return
        }
        if text.hasPrefix(":") {
            out.append(.comment(String(text.dropFirst()).trimmingCharacters(in: .whitespaces)))
            return
        }
        let field: Substring
        var value: Substring
        if let colon = text.firstIndex(of: ":") {
            field = text[..<colon]
            value = text[text.index(after: colon)...]
            if value.first == " " { value = value.dropFirst() }
        } else {
            field = Substring(text)
            value = ""
        }
        switch field {
        case "event":
            eventType = String(value)
        case "data":
            data += value
            data += "\n"
            hasData = true
        case "id":
            if !value.contains("\u{0}") { lastEventID = String(value) }
        case "retry":
            if !value.isEmpty, value.allSatisfy(\.isASCII), value.allSatisfy(\.isNumber), let ms = Int(value) {
                out.append(.retry(ms))
            }
        default:
            break
        }
    }

    private mutating func dispatch(into out: inout [SSEOutput]) {
        defer {
            data = ""
            eventType = ""
            hasData = false
        }
        guard hasData else { return }
        if data.hasSuffix("\n") { data.removeLast() }
        out.append(.event(SSEEvent(id: lastEventID, event: eventType.isEmpty ? "message" : eventType, data: data)))
    }
}
