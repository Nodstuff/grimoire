import Charts
import SwiftUI
import TaisceKit

/// The doc's tables, for `vega-lite` fences that chart one (`{"block": "^abc123"}`).
extension EnvironmentValues {
    @Entry var docTables = ChartTables()
}

extension DocPage {
    var chartTables: ChartTables { ChartTables(blocks: blocks.map { (id: $0.id, nodes: $0.nodes) }) }
}

/// Series colors: the accent first, then hues that hold AA-ish contrast on
/// the surface in both appearances (light values are the darker twins).
enum ChartPalette {
    static let colors: [Color] = [
        Theme.accent,
        Theme.green,
        Theme.amber,
        Theme.rose,
        Color(dark: 0x7FC4C4, light: 0x2F7676),
        Color(dark: 0xB79CD9, light: 0x6B4E9E),
        Color(dark: 0xC9A27E, light: 0x7E5634),
        Color(dark: 0xA3A3B0, light: 0x5E5E6C),
    ]

    static func colors(for series: [String]) -> [Color] {
        series.indices.map { colors[$0 % colors.count] }
    }
}

/// A ```` ```vega-lite ```` fence: parsed against the doc's tables, drawn
/// natively, or a tidy card saying why not.
struct ChartBlock: View {
    @Environment(\.docTables) private var tables
    let source: String

    var body: some View {
        switch ChartSpec.parse(source, tables: tables) {
        case let .chart(model):
            ChartModelView(model: model)
        case let .unsupported(mark, reason):
            DiagramNoticeCard(
                icon: "chart.xyaxis.line", title: "Chart type not supported on iPhone yet",
                detail: "Mark: \(mark) · uses \(reason). It renders in the desktop app."
            )
        case let .noData(mark, note):
            DiagramNoticeCard(icon: "tablecells", title: "No data for this \(mark) chart", detail: note)
        case let .invalid(message, line, excerpt):
            DiagramNoticeCard(
                icon: "exclamationmark.triangle", title: "Chart spec isn't valid JSON",
                detail: line.map { "Line \($0): \(message)" } ?? message,
                code: excerpt, tint: Theme.amber
            )
        }
    }
}

/// `ChartModel` → Swift Charts. Axis value types follow the model's field
/// types (quantitative → Double, temporal → Date, nominal → String).
struct ChartModelView: View {
    let model: ChartModel

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            if let title = model.title {
                Text(title).font(.subheadline.weight(.semibold)).foregroundStyle(Theme.text)
            }
            chart
                .frame(height: model.height)
        }
        .padding(14)
        .card(Theme.surface)
        .accessibilityElement(children: .contain)
        .accessibilityLabel(model.summary)
    }

    @ViewBuilder private var chart: some View {
        if model.isArc {
            Chart {
                ForEach(Array((model.layers.first?.points ?? []).enumerated()), id: \.offset) { _, p in
                    SectorMark(
                        angle: .value(model.colorTitle ?? "Value", p.y?.number ?? 0),
                        innerRadius: .ratio(model.layers.first?.innerRadius ?? 0),
                        angularInset: 1.5
                    )
                    .cornerRadius(3)
                    .foregroundStyle(by: .value(model.colorTitle ?? "Category", p.series ?? ""))
                    .accessibilityLabel(p.series ?? "")
                    .accessibilityValue(p.y?.text ?? "")
                }
            }
            .chartForegroundStyleScale(domain: model.series, range: ChartPalette.colors(for: model.series))
            .chartLegend(position: .bottom, alignment: .leading)
        } else {
            Chart {
                ForEach(Array(model.layers.enumerated()), id: \.offset) { _, layer in
                    ForEach(Array(layer.points.enumerated()), id: \.offset) { _, p in
                        ChartMarkContent(model: model, layer: layer, point: p)
                    }
                }
            }
            .modifier(ChartScales(model: model))
            .chartXAxis { axis(.x) }
            .chartYAxis { axis(.y) }
            .chartXAxisLabel(model.x?.title ?? "")
            .chartYAxisLabel(model.y?.title ?? "")
            .chartLegend(model.series.count > 1 ? .visible : .hidden)
        }
    }

    enum Which { case x, y }

    @AxisContentBuilder func axis(_ which: Which) -> some AxisContent {
        AxisMarks { _ in
            AxisGridLine().foregroundStyle(Theme.hairline)
            AxisTick().foregroundStyle(Theme.hairline)
            AxisValueLabel().foregroundStyle(Theme.secondary)
        }
    }
}

/// Category domains, the series colors and horizontal scrolling, each only
/// when the model asks for it.
private struct ChartScales: ViewModifier {
    let model: ChartModel

    func body(content: Content) -> some View {
        content
            .modifier(XDomain(domain: model.x?.domain))
            .modifier(YDomain(domain: model.y?.domain))
            .modifier(SeriesColors(series: model.series))
            .modifier(Scrolling(model: model))
    }

    struct XDomain: ViewModifier {
        let domain: [String]?
        func body(content: Content) -> some View {
            if let domain { content.chartXScale(domain: domain) } else { content }
        }
    }

    struct YDomain: ViewModifier {
        let domain: [String]?
        func body(content: Content) -> some View {
            if let domain { content.chartYScale(domain: domain) } else { content }
        }
    }

    struct SeriesColors: ViewModifier {
        let series: [String]
        func body(content: Content) -> some View {
            if series.isEmpty {
                content
            } else {
                content.chartForegroundStyleScale(domain: series, range: ChartPalette.colors(for: series))
            }
        }
    }

    struct Scrolling: ViewModifier {
        let model: ChartModel

        func body(content: Content) -> some View {
            if let visible = model.visibleXCount, let x = model.x {
                switch x.type {
                case .temporal:
                    content.chartScrollableAxes(.horizontal).chartXVisibleDomain(length: visibleSpan(visible))
                default:
                    content.chartScrollableAxes(.horizontal).chartXVisibleDomain(length: visible)
                }
            } else {
                content
            }
        }

        /// the time `visible` of the series' x values cover, on average
        func visibleSpan(_ visible: Int) -> TimeInterval {
            let dates = model.layers.flatMap { $0.points.compactMap { $0.x?.date } }
            guard let lo = dates.min(), let hi = dates.max(), dates.count > 1 else { return 86_400 * Double(visible) }
            let distinct = Set(dates).count
            return hi.timeIntervalSince(lo) * Double(visible) / Double(max(distinct - 1, 1))
        }
    }
}

/// One datum as a mark, typed by the axes.
private struct ChartMarkContent: ChartContent {
    let model: ChartModel
    let layer: ChartLayer
    let point: ChartPoint

    var xTitle: String { model.x?.title ?? "x" }
    var yTitle: String { model.y?.title ?? "y" }

    var body: some ChartContent {
        if let x = point.x {
            if let y = point.y { both(x, y) } else { xRule(x) }
        } else if let y = point.y {
            yRule(y)
        } else {
            RuleMark(y: .value(yTitle, 0.0)).opacity(0)
        }
    }

    // split up so the type checker keeps up

    @ChartContentBuilder func both(_ x: ChartValue, _ y: ChartValue) -> some ChartContent {
        switch x {
        case let .text(a): withY(a, y)
        case let .number(a): withY(a, y)
        case let .date(a): withY(a, y)
        }
    }

    @ChartContentBuilder func withY<X: Plottable>(_ a: X, _ y: ChartValue) -> some ChartContent {
        switch y {
        case let .number(b): plot(a, b)
        case let .text(b): plot(a, b)
        case let .date(b): plot(a, b)
        }
    }

    @ChartContentBuilder func xRule(_ x: ChartValue) -> some ChartContent {
        switch x {
        case let .number(a): RuleMark(x: .value(xTitle, a)).styled(layer, point, rule: true)
        case let .date(a): RuleMark(x: .value(xTitle, a)).styled(layer, point, rule: true)
        case let .text(a): RuleMark(x: .value(xTitle, a)).styled(layer, point, rule: true)
        }
    }

    @ChartContentBuilder func yRule(_ y: ChartValue) -> some ChartContent {
        switch y {
        case let .number(b): RuleMark(y: .value(yTitle, b)).styled(layer, point, rule: true)
        case let .date(b): RuleMark(y: .value(yTitle, b)).styled(layer, point, rule: true)
        case let .text(b): RuleMark(y: .value(yTitle, b)).styled(layer, point, rule: true)
        }
    }

    var stacking: MarkStackingMethod {
        switch layer.stacking {
        case .standard: .standard
        case .normalized: .normalized
        case .center: .center
        case .none: .unstacked
        }
    }

    var interpolation: InterpolationMethod {
        switch layer.interpolation {
        case .linear: .linear
        case .monotone: .monotone
        case .step: .stepCenter
        case .catmullRom: .catmullRom
        }
    }

    /// ticks run across the continuous axis
    var tickAcrossX: Bool { model.y?.type == .quantitative && model.x?.type != .quantitative }

    @ChartContentBuilder func plot<X: Plottable, Y: Plottable>(_ x: X, _ y: Y) -> some ChartContent {
        let xv = PlottableValue.value(xTitle, x), yv = PlottableValue.value(yTitle, y)
        switch layer.mark {
        case .bar:
            BarMark(x: xv, y: yv, stacking: stacking).grouped(layer, point).styled(layer, point)
        case .line:
            LineMark(x: xv, y: yv).interpolationMethod(interpolation).lineStyle(StrokeStyle(lineWidth: 2.2, lineCap: .round)).styled(layer, point)
        case .area:
            AreaMark(x: xv, y: yv, stacking: stacking).interpolationMethod(interpolation).opacity(0.8).styled(layer, point)
        case .point:
            if layer.filled {
                PointMark(x: xv, y: yv).symbol(.circle).symbolSize(CGFloat(point.size ?? 50)).styled(layer, point)
            } else {
                PointMark(x: xv, y: yv).symbol(BasicChartSymbolShape.circle.strokeBorder(lineWidth: 1.5)).symbolSize(CGFloat(point.size ?? 50)).styled(layer, point)
            }
        case .tick:
            RectangleMark(x: xv, y: yv, width: .fixed(tickAcrossX ? 16 : 2), height: .fixed(tickAcrossX ? 2 : 16)).styled(layer, point)
        case .rule, .arc:
            PointMark(x: xv, y: yv).styled(layer, point)
        }
    }
}

private extension ChartContent {
    /// a colored layer takes its series' color (the scale), others the accent;
    /// a rule over colored marks is drawn in the text color
    @ChartContentBuilder func styled(_ layer: ChartLayer, _ p: ChartPoint, rule: Bool = false) -> some ChartContent {
        if layer.colored, let s = p.series {
            self.foregroundStyle(by: .value("Series", s))
                .accessibilityLabel([p.x?.text, s].compactMap { $0 }.joined(separator: ", "))
                .accessibilityValue(p.y?.text ?? "")
        } else {
            self.foregroundStyle(rule ? Theme.secondary : Theme.accent)
                .accessibilityLabel(p.x?.text ?? "")
                .accessibilityValue(p.y?.text ?? "")
        }
    }

    @ChartContentBuilder func grouped(_ layer: ChartLayer, _ p: ChartPoint) -> some ChartContent {
        if layer.grouped, let s = p.series {
            self.position(by: .value("Group", s))
        } else {
            self
        }
    }
}

/// A diagram that can't be drawn here: why, in one or two lines.
struct DiagramNoticeCard: View {
    let icon: String
    let title: String
    let detail: String
    var code: String?
    var tint: Color = Theme.accent

    var body: some View {
        HStack(alignment: .top, spacing: 14) {
            Image(systemName: icon)
                .font(.title3)
                .foregroundStyle(tint)
                .frame(width: 40, height: 40)
                .background(tint.opacity(0.12), in: .rect(cornerRadius: 10, style: .continuous))
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 4) {
                Text(title).font(.subheadline.weight(.medium)).foregroundStyle(Theme.text)
                Text(detail).font(.footnote).foregroundStyle(Theme.secondary).fixedSize(horizontal: false, vertical: true)
                if let code, !code.isEmpty {
                    Text(code).font(Theme.mono).foregroundStyle(Theme.text).lineLimit(2).padding(.top, 2)
                }
            }
            Spacer(minLength: 0)
        }
        .padding(14)
        .card(Theme.surface2)
        .accessibilityElement(children: .combine)
    }
}

#if DEBUG
#Preview("Charts") {
    ScrollView {
        VStack(spacing: 16) {
            ChartBlock(source: #"{"title": "Revenue", "data": {"values": [{"m": "Jan", "r": "EU", "v": 4}, {"m": "Jan", "r": "US", "v": 3}, {"m": "Feb", "r": "EU", "v": 6}, {"m": "Feb", "r": "US", "v": 2}]}, "mark": "bar", "encoding": {"x": {"field": "m", "sort": null}, "y": {"field": "v", "type": "quantitative"}, "color": {"field": "r"}}}"#)
            ChartBlock(source: #"{"data": {"values": [{"c": "a", "v": 4}, {"c": "b", "v": 6}, {"c": "c", "v": 3}]}, "mark": {"type": "arc", "innerRadius": 60}, "encoding": {"theta": {"field": "v", "type": "quantitative"}, "color": {"field": "c"}}}"#)
            ChartBlock(source: #"{"mark": "rect"}"#)
            ChartBlock(source: "{\n \"mark\": bar\n}")
        }
        .padding()
    }
    .groundBackground()
    .preferredColorScheme(.dark)
}
#endif
