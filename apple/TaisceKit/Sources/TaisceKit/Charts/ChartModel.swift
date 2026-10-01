import Foundation

/// A Vega-Lite spec reduced to what the phone draws natively (Swift Charts).
/// Pure data: `ChartSpec.parse` builds it, the app's chart view renders it.
public struct ChartModel: Sendable, Hashable {
    public var title: String?
    /// nil for arcs (no axes) and for a rule-only chart on the other axis
    public var x: ChartAxis?
    public var y: ChartAxis?
    /// the legend's title (the color field)
    public var colorTitle: String?
    /// distinct color values, in legend order; empty = one color
    public var series: [String]
    public var layers: [ChartLayer]
    /// points plot height, clamped to 220...320
    public var height: Double
    /// a wide temporal (or long categorical) x scrolls; this many x values fit
    public var visibleXCount: Int?
    /// what VoiceOver says for the chart as a whole
    public var summary: String

    public var isArc: Bool { layers.contains { $0.mark == .arc } }
}

public enum ChartFieldType: String, Sendable, Hashable {
    case quantitative, temporal, nominal, ordinal

    public var isDiscrete: Bool { self == .nominal || self == .ordinal }
}

public struct ChartAxis: Sendable, Hashable {
    public var title: String
    public var type: ChartFieldType
    /// explicit category order for a discrete axis (from `sort`); nil = data order
    public var domain: [String]?
}

public enum ChartMark: String, Sendable, Hashable, CaseIterable {
    case bar, line, area, point, arc, rule, tick
}

public enum ChartStacking: String, Sendable, Hashable {
    case standard, normalized, center, none
}

public enum ChartInterpolation: String, Sendable, Hashable {
    case linear, monotone, step, catmullRom
}

public struct ChartLayer: Sendable, Hashable {
    public var mark: ChartMark
    public var points: [ChartPoint]
    public var stacking: ChartStacking = .standard
    /// bars side by side per series (`xOffset`), not stacked
    public var grouped = false
    /// arc only: 0 = pie, otherwise the inner radius as a ratio of the outer
    public var innerRadius: Double = 0
    public var interpolation: ChartInterpolation = .linear
    /// point: filled (the default) or outlined
    public var filled = true
    /// true when the layer has a color field (else it takes the accent)
    public var colored = false
}

/// One datum. For arcs `y` is the slice's size and `series` its category.
public struct ChartPoint: Sendable, Hashable {
    public var x: ChartValue?
    public var y: ChartValue?
    public var series: String?
    /// point area in pt² (from `size`); nil = the default
    public var size: Double?
}

public enum ChartValue: Sendable, Hashable, Comparable {
    case number(Double)
    case date(Date)
    case text(String)

    public var number: Double? {
        if case let .number(n) = self { return n }
        return nil
    }

    public var date: Date? {
        if case let .date(d) = self { return d }
        return nil
    }

    /// for labels and categorical axes
    public var text: String {
        switch self {
        case let .number(n): ChartValue.format(n)
        case let .date(d): ChartValue.dayFormatter.string(from: d)
        case let .text(s): s
        }
    }

    static func format(_ n: Double) -> String {
        if n.rounded() == n, abs(n) < 1e15 { return String(Int64(n)) }
        return String(format: "%g", n)
    }

    nonisolated(unsafe) private static let dayFormatter: ISO8601DateFormatter = {
        let f = ISO8601DateFormatter()
        f.formatOptions = [.withFullDate]
        f.timeZone = .current
        return f
    }()

    public static func < (a: ChartValue, b: ChartValue) -> Bool {
        switch (a, b) {
        case let (.number(x), .number(y)): x < y
        case let (.date(x), .date(y)): x < y
        default:
            if let x = Double(a.text), let y = Double(b.text) { x < y } else { a.text.localizedStandardCompare(b.text) == .orderedAscending }
        }
    }
}

/// What a `vega-lite` fence becomes.
public enum ChartSpecResult: Sendable, Hashable {
    case chart(ChartModel)
    /// a mark or feature the phone can't draw; `mark` names it for the card
    case unsupported(mark: String, reason: String)
    /// the JSON doesn't parse: the error, its 1-based line and that line's text
    case invalid(message: String, line: Int?, excerpt: String?)
    /// a valid spec whose data the phone can't get (a URL, a missing table)
    case noData(mark: String, note: String)
}
