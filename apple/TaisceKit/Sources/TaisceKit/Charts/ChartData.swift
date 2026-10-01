import Foundation

/// A JSON value with its numbers and booleans told apart (Foundation's
/// `NSNumber` holds both).
public enum JSONValue: Sendable, Hashable {
    case null
    case bool(Bool)
    case number(Double)
    case string(String)
    case array([JSONValue])
    case object([String: JSONValue])

    init(_ any: Any) {
        switch any {
        case let n as NSNumber:
            self = CFGetTypeID(n) == CFBooleanGetTypeID() ? .bool(n.boolValue) : .number(n.doubleValue)
        case let s as String: self = .string(s)
        case let a as [Any]: self = .array(a.map(JSONValue.init))
        case let o as [String: Any]: self = .object(o.mapValues(JSONValue.init))
        default: self = .null
        }
    }

    subscript(key: String) -> JSONValue? {
        if case let .object(o) = self { return o[key] }
        return nil
    }

    var string: String? {
        if case let .string(s) = self { return s }
        return nil
    }

    var number: Double? {
        switch self {
        case let .number(n): n
        case let .string(s): ChartCoercion.number(s)
        default: nil
        }
    }

    var array: [JSONValue]? {
        if case let .array(a) = self { return a }
        return nil
    }

    var object: [String: JSONValue]? {
        if case let .object(o) = self { return o }
        return nil
    }

    var isNull: Bool { self == .null }
    var isFalse: Bool { self == .bool(false) }
}

/// The GFM tables in one doc, found by block ref (`^abc123`, the last six
/// hex digits of the block id, or the whole id), for table-backed charts.
public struct ChartTables: Sendable, Hashable {
    /// block ids and their first tables, in doc order
    private var ids: [BlockID] = []
    private var items: [RenderNode.Table] = []

    public init() {}

    public init(blocks: [(id: BlockID, nodes: [RenderNode])]) {
        for b in blocks {
            if let t = Self.firstTable(b.nodes) {
                ids.append(b.id)
                items.append(t)
            }
        }
    }

    public func table(ref: String) -> RenderNode.Table? {
        var r = ref.trimmingCharacters(in: .whitespaces).lowercased()
        if r.hasPrefix("^") { r.removeFirst() }
        let bare = r.replacingOccurrences(of: "-", with: "")
        guard !bare.isEmpty else { return nil }
        for (id, t) in zip(ids, items) {
            let hex = id.lowercased().replacingOccurrences(of: "-", with: "")
            if hex == bare || (bare.count >= 6 && hex.hasSuffix(bare)) { return t }
        }
        return nil
    }

    static func firstTable(_ nodes: [RenderNode]) -> RenderNode.Table? {
        for n in nodes {
            switch n {
            case let .table(t): return t
            case let .quote(_, children): if let t = firstTable(children) { return t }
            default: continue
            }
        }
        return nil
    }

    public static func == (a: ChartTables, b: ChartTables) -> Bool { a.ids == b.ids && a.items == b.items }
    public func hash(into h: inout Hasher) {
        h.combine(ids)
        h.combine(items)
    }
}

/// Table cells and loose strings → numbers and dates.
enum ChartCoercion {
    /// "1,234", "12%", "$5", " 3.5 " → numbers; anything else nil
    static func number(_ raw: String) -> Double? {
        var s = raw.trimmingCharacters(in: .whitespaces)
        guard !s.isEmpty else { return nil }
        if let f = s.first, "$€£¥".contains(f) { s.removeFirst() }
        if s.hasSuffix("%") { s.removeLast() }
        s = s.replacingOccurrences(of: ",", with: "")
        guard let n = Double(s), n.isFinite else { return nil }
        // "1e5" and "inf" are numbers to Double; "e5" is not a cell anyone means as one
        guard s.first.map({ $0.isNumber || $0 == "-" || $0 == "+" || $0 == "." }) == true else { return nil }
        return n
    }

    /// ISO dates: 2026, 2026-10, 2026-10-01, 2026-10-01T14:00[:00[.sss]][Z|±hh:mm].
    /// A date without a zone is local time (as Vega-Lite reads it), one with
    /// a zone is that instant.
    static func date(_ raw: String, timeZone: TimeZone = .current) -> Date? {
        let s = raw.trimmingCharacters(in: .whitespaces)
        let pattern = #"^(\d{4})(?:-(\d{2})(?:-(\d{2})(?:[T ](\d{2}):(\d{2})(?::(\d{2})(?:\.(\d+))?)?(Z|[+-]\d{2}:?\d{2})?)?)?)?$"#
        guard let m = s.firstMatch(of: try! Regex(pattern)) else { return nil }
        func part(_ i: Int) -> String? { m.output[i].substring.map(String.init) }
        var c = DateComponents()
        c.year = part(1).flatMap { Int($0) }
        c.month = part(2).flatMap { Int($0) } ?? 1
        c.day = part(3).flatMap { Int($0) } ?? 1
        c.hour = part(4).flatMap { Int($0) } ?? 0
        c.minute = part(5).flatMap { Int($0) } ?? 0
        c.second = part(6).flatMap { Int($0) } ?? 0
        if let frac = part(7) { c.nanosecond = Int((Double("0." + frac) ?? 0) * 1e9) }
        guard let y = c.year, (1...12).contains(c.month!), (1...31).contains(c.day!), y > 0 else { return nil }
        var cal = Calendar(identifier: .gregorian)
        cal.timeZone = timeZone
        if let zone = part(8) {
            if zone == "Z" {
                cal.timeZone = TimeZone(identifier: "UTC")!
            } else {
                let digits = zone.dropFirst().replacingOccurrences(of: ":", with: "")
                let h = Int(digits.prefix(2)) ?? 0, mm = Int(digits.suffix(2)) ?? 0
                let offset = (h * 3600 + mm * 60) * (zone.hasPrefix("-") ? -1 : 1)
                guard let tz = TimeZone(secondsFromGMT: offset) else { return nil }
                cal.timeZone = tz
            }
        }
        return cal.date(from: c)
    }

    /// A table cell's inline markdown as plain text: emphasis, code ticks,
    /// wikilink brackets and escapes go.
    static func plain(_ inline: String) -> String {
        var s = inline
        s = s.replacingOccurrences(of: #"\[\[([^\]|]*\|)?([^\]]*)\]\]"#, with: "$2", options: .regularExpression)
        s = s.replacingOccurrences(of: #"\[([^\]]*)\]\([^)]*\)"#, with: "$1", options: .regularExpression)
        for token in ["**", "__", "`", "~~"] { s = s.replacingOccurrences(of: token, with: "") }
        s = s.replacingOccurrences(of: #"(?<![\w*])[*_](?!\s)([^*_]+)[*_]"#, with: "$1", options: .regularExpression)
        s = s.replacingOccurrences(of: #"\\(.)"#, with: "$1", options: .regularExpression)
        return s.trimmingCharacters(in: .whitespaces)
    }

    /// Rows of a GFM table: header cells are the fields; a cell that reads
    /// as a number is one, an empty cell is null, the rest stay text (ISO
    /// dates are read as dates where the field turns out temporal).
    static func rows(_ table: RenderNode.Table) -> [[String: JSONValue]] {
        let fields = table.header.enumerated().map { i, h -> String in
            let p = plain(h)
            return p.isEmpty ? "column \(i + 1)" : p
        }
        return table.rows.map { row in
            var out: [String: JSONValue] = [:]
            for (i, f) in fields.enumerated() {
                let cell = i < row.count ? plain(row[i]) : ""
                out[f] = cell.isEmpty ? .null : number(cell).map(JSONValue.number) ?? .string(cell)
            }
            return out
        }
    }
}
