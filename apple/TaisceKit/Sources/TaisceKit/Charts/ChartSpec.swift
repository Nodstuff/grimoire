import Foundation

/// Vega-Lite JSON → `ChartModel`, for the subset the phone draws natively:
/// marks bar, line, area, point, arc, rule, tick; encodings x, y, color,
/// theta, size, xOffset; `aggregate` (sum, mean, count, min, max, median);
/// `sort`; `stack`; titles; one level of `layer`. Data is inline `values`
/// or a GFM table in the same doc (`{"block": "^abc123"}` /
/// `{"table": "^abc123"}`). Everything else is `.unsupported`, never a guess.
public enum ChartSpec {
    static let supportedAggregates: Set<String> = ["sum", "mean", "average", "count", "min", "max", "median"]
    static let facetKeys = ["facet", "repeat", "concat", "hconcat", "vconcat"]
    static let facetChannels = ["row", "column", "facet"]
    /// beyond this many x values a temporal or categorical x scrolls
    static let scrollThreshold = 24
    static let visibleWhenScrolling = 16

    public static func parse(_ source: String, tables: ChartTables = ChartTables()) -> ChartSpecResult {
        let root: JSONValue
        do {
            root = JSONValue(try JSONSerialization.jsonObject(with: Data(source.utf8)))
        } catch {
            return invalid(error, source: source)
        }
        guard case .object = root else {
            return .invalid(message: "A Vega-Lite spec is a JSON object.", line: 1, excerpt: firstLine(source))
        }
        do {
            return try build(root, tables: tables)
        } catch let u as Unsupported {
            return u.result
        } catch {
            return .unsupported(mark: "chart", reason: "\(error)")
        }
    }

    // MARK: - errors

    struct Unsupported: Error {
        var result: ChartSpecResult

        static func mark(_ m: String, _ reason: String) -> Unsupported {
            Unsupported(result: .unsupported(mark: m, reason: reason))
        }
    }

    static func invalid(_ error: any Error, source: String) -> ChartSpecResult {
        let ns = error as NSError
        var message = (ns.userInfo[NSDebugDescriptionErrorKey] as? String) ?? ns.localizedDescription
        if let r = message.range(of: " around line") { message = String(message[..<r.lowerBound]) }
        message = message.trimmingCharacters(in: CharacterSet(charactersIn: ". "))
        if message.isEmpty { message = "Invalid JSON" }
        guard let index = ns.userInfo["NSJSONSerializationErrorIndex"] as? Int else {
            return .invalid(message: message, line: nil, excerpt: nil)
        }
        let bytes = Array(source.utf8)
        let upTo = bytes.prefix(max(0, min(index, bytes.count)))
        let line = upTo.reduce(1) { $0 + ($1 == UInt8(ascii: "\n") ? 1 : 0) }
        let lines = source.split(separator: "\n", omittingEmptySubsequences: false)
        let excerpt = line - 1 < lines.count ? lines[line - 1].trimmingCharacters(in: .whitespaces) : nil
        return .invalid(message: message, line: line, excerpt: excerpt)
    }

    static func firstLine(_ s: String) -> String? {
        s.split(separator: "\n").first.map { String($0.prefix(80)) }
    }

    // MARK: - spec

    struct Channel {
        var field: String?
        var type: ChartFieldType?
        var aggregate: String?
        var title: String?
        var sort: JSONValue?
        var stack: JSONValue?
        var datum: JSONValue?
        var timeUnit: Bool

        init?(_ v: JSONValue?) {
            guard let o = v?.object else { return nil }
            field = o["field"]?.string
            type = o["type"]?.string.flatMap(ChartFieldType.init(rawValue:))
            aggregate = o["aggregate"]?.string?.lowercased()
            title = o["title"]?.string ?? o["axis"]?["title"]?.string ?? o["legend"]?["title"]?.string
            sort = o["sort"]
            stack = o["stack"]
            datum = o["datum"] ?? o["value"]
            timeUnit = o["timeUnit"] != nil
            if field == nil, aggregate == nil, datum == nil { return nil }
        }

        /// "Sum of sales", or the field, or "Count"
        var label: String {
            if let title { return title }
            switch (aggregate, field) {
            case let (agg?, f?): return "\(Self.aggregateName(agg)) of \(f)"
            case ("count", nil): return "Count"
            case let (nil, f?): return f
            default: return ""
            }
        }

        static func aggregateName(_ a: String) -> String {
            switch a {
            case "sum": "Sum"
            case "mean", "average": "Mean"
            case "count": "Count"
            case "min": "Min"
            case "max": "Max"
            case "median": "Median"
            default: a.capitalized
            }
        }
    }

    struct LayerSpec {
        var mark: ChartMark
        var markName: String
        var markDef: [String: JSONValue]
        var encoding: [String: JSONValue]
        var data: JSONValue?
    }

    static func markOf(_ v: JSONValue?) -> (name: String, def: [String: JSONValue])? {
        if let s = v?.string { return (s, [:]) }
        if let o = v?.object, let t = o["type"]?.string { return (t, o) }
        return nil
    }

    static func build(_ root: JSONValue, tables: ChartTables) throws -> ChartSpecResult {
        for k in facetKeys where root[k] != nil {
            throw Unsupported.mark(k, "\(k == "facet" || k == "repeat" ? "faceted" : "concatenated") views")
        }
        let topMark = markOf(root["mark"])
        let topEncoding = root["encoding"]?.object ?? [:]
        var specs: [LayerSpec] = []
        if let layers = root["layer"]?.array {
            for l in layers {
                if l["layer"] != nil { throw Unsupported.mark("layer", "nested layers") }
                guard let m = markOf(l["mark"]) else { throw Unsupported.mark("layer", "a layer without a mark") }
                let enc = topEncoding.merging(l["encoding"]?.object ?? [:]) { _, new in new }
                specs.append(try layerSpec(m, encoding: enc, data: l["data"] ?? root["data"], transform: l["transform"]))
            }
        } else if let m = topMark {
            specs.append(try layerSpec(m, encoding: topEncoding, data: root["data"], transform: nil))
        } else {
            throw Unsupported.mark("chart", "a spec without a mark")
        }
        if let t = root["transform"], t.array?.isEmpty == false {
            throw Unsupported.mark(specs.first?.markName ?? "chart", "transforms")
        }
        guard !specs.isEmpty else { throw Unsupported.mark("layer", "an empty layer list") }
        let mainMark = specs[0].markName
        if specs.contains(where: { $0.mark == .arc }), specs.count > 1 {
            throw Unsupported.mark("arc", "layered arcs")
        }

        // data per layer
        var datasets: [[[String: JSONValue]]] = []
        for s in specs {
            switch try rows(s.data, tables: tables) {
            case let .rows(r): datasets.append(r)
            case let .missing(note): return .noData(mark: mainMark, note: note)
            }
        }

        var model = ChartModel(title: title(root["title"]), x: nil, y: nil, colorTitle: nil, series: [], layers: [], height: 260, visibleXCount: nil, summary: "")
        if let h = root["height"]?.number { model.height = min(320, max(220, h)) }

        var series: [String] = []
        func addSeries(_ s: String) { if !series.contains(s) { series.append(s) } }

        for (s, data) in zip(specs, datasets) {
            let enc = s.encoding
            let x = Channel(enc["x"]), y = Channel(enc["y"]), color = Channel(enc["color"])
            let theta = Channel(enc["theta"]), size = Channel(enc["size"]), xOffset = Channel(enc["xOffset"])
            var layer = ChartLayer(mark: s.mark, points: [])

            if s.mark == .arc {
                guard let theta else { throw Unsupported.mark("arc", "an arc without theta") }
                let measure = Measure(theta)
                let groups = try aggregate(data, dims: [color?.field].compactMap { $0 }, measure: measure)
                for g in groups {
                    guard let v = g.value else { continue }
                    let cat = color?.field.flatMap { g.dims[$0] }.map(text) ?? measure.label
                    layer.points.append(ChartPoint(x: nil, y: .number(v), series: cat))
                    addSeries(cat)
                }
                layer.colored = color != nil
                let inner = s.markDef["innerRadius"]?.number ?? 0
                let outer = s.markDef["outerRadius"]?.number ?? (model.height / 2 - 10)
                layer.innerRadius = inner > 0 ? min(0.85, max(0.2, inner / max(outer, 1))) : 0
                model.colorTitle = color?.label
                model.layers.append(layer)
                continue
            }

            if theta != nil { throw Unsupported.mark(s.markName, "theta on a \(s.markName) mark") }
            for c in facetChannels where enc[c] != nil { throw Unsupported.mark(s.markName, "faceted views") }
            for c in ["x2", "y2"] where enc[c] != nil { throw Unsupported.mark(s.markName, "ranged \(c.dropLast()) marks") }
            if x == nil, y == nil { throw Unsupported.mark(s.markName, "a mark without x or y") }
            if s.mark == .rule, x != nil, y != nil, x?.datum == nil, y?.datum == nil {
                throw Unsupported.mark("rule", "rules with both x and y")
            }

            // which channel is the measure (aggregated) and which are dimensions
            let positional: [(String, Channel?)] = [("x", x), ("y", y)]
            let aggregated = positional.filter { $0.1?.aggregate != nil }
            if aggregated.count > 1 { throw Unsupported.mark(s.markName, "aggregating both x and y") }
            for (_, c) in positional { if let a = c?.aggregate, !supportedAggregates.contains(a) { throw Unsupported.mark(s.markName, "the \(a) aggregate") } }
            if let a = size?.aggregate ?? color?.aggregate, !supportedAggregates.contains(a) { throw Unsupported.mark(s.markName, "the \(a) aggregate") }

            var points: [(x: JSONValue?, y: JSONValue?, series: String?, size: Double?)] = []
            let seriesField = color?.field ?? xOffset?.field
            if let (axisName, m) = aggregated.first, let mc = m {
                let other = axisName == "x" ? y : x
                let dims = [other?.field, seriesField].compactMap { $0 }
                let groups = try aggregate(data, dims: dims, measure: Measure(mc))
                for g in groups {
                    let dimValue = other.flatMap { o in o.field.map { g.dims[$0] ?? .null } ?? o.datum }
                    let measure = g.value.map(JSONValue.number)
                    let sv = seriesField.flatMap { g.dims[$0] }.map(text)
                    points.append(axisName == "x" ? (measure, dimValue, sv, nil) : (dimValue, measure, sv, nil))
                }
            } else {
                for row in data {
                    let xv = x.flatMap { c in c.field.map { row[$0] ?? .null } ?? c.datum }
                    let yv = y.flatMap { c in c.field.map { row[$0] ?? .null } ?? c.datum }
                    let sv = seriesField.flatMap { row[$0] }.flatMap { $0.isNull ? nil : text($0) }
                    let sz = size?.field.flatMap { row[$0]?.number }
                    points.append((xv, yv, sv, sz))
                }
                if data.isEmpty, x?.field == nil, y?.field == nil {
                    // a rule at a datum needs no data
                    points.append((x?.datum, y?.datum, nil, nil))
                }
            }

            // axis types: the first layer that has the channel decides
            if let x {
                let t = fieldType(x, values: points.compactMap(\.x))
                if let have = model.x?.type, kind(have) != kind(t) { throw Unsupported.mark(s.markName, "layers with different x types") }
                if model.x == nil { model.x = ChartAxis(title: x.label, type: t, domain: nil) }
            }
            if let y {
                let t = fieldType(y, values: points.compactMap(\.y))
                if let have = model.y?.type, kind(have) != kind(t) { throw Unsupported.mark(s.markName, "layers with different y types") }
                if model.y == nil { model.y = ChartAxis(title: y.label, type: t, domain: nil) }
            }

            // sizes → point areas, 20...300 pt²
            let sizes = points.compactMap(\.size)
            let lo = sizes.min() ?? 0, hi = sizes.max() ?? 0
            for p in points {
                guard let xv = coerce(p.x, model.x?.type, present: x != nil), let yv = coerce(p.y, model.y?.type, present: y != nil) else { continue }
                var area: Double?
                if let sz = p.size { area = hi > lo ? 20 + (sz - lo) / (hi - lo) * 280 : 80 }
                layer.points.append(ChartPoint(x: x == nil ? nil : xv, y: y == nil ? nil : yv, series: p.series, size: area))
                if let sv = p.series { addSeries(sv) }
            }

            layer.colored = seriesField != nil
            layer.grouped = xOffset != nil && s.mark == .bar
            let stackSource = (y?.aggregate != nil || model.y?.type == .quantitative) ? y?.stack : x?.stack
            layer.stacking = stacking(stackSource, mark: s.mark, grouped: layer.grouped)
            layer.interpolation = interpolation(s.markDef["interpolate"]?.string)
            layer.filled = !(s.markDef["filled"]?.isFalse ?? false)
            if color != nil, model.colorTitle == nil { model.colorTitle = color?.label }
            model.layers.append(layer)

            // `"point": true` on a line adds a point layer
            if s.mark == .line, let p = s.markDef["point"], p != .bool(false), !p.isNull {
                var dots = ChartLayer(mark: .point, points: layer.points)
                dots.colored = layer.colored
                model.layers.append(dots)
            }

            // sort order of a discrete axis
            if let x, model.x?.type.isDiscrete == true, model.x?.domain == nil {
                model.x?.domain = order(x.sort, values: layer.points.compactMap(\.x), measures: layer.points.map(\.y), measureChannel: "y")
            }
            if let y, model.y?.type.isDiscrete == true, model.y?.domain == nil {
                model.y?.domain = order(y.sort, values: layer.points.compactMap(\.y), measures: layer.points.map(\.x), measureChannel: "x")
            }
        }

        model.series = model.layers.contains(where: \.colored) ? series : []
        if let x = model.x, x.type == .temporal || x.type.isDiscrete, !model.isArc {
            let distinct = Set(model.layers.flatMap { $0.points.compactMap(\.x) }).count
            if distinct > scrollThreshold { model.visibleXCount = visibleWhenScrolling }
        }
        model.summary = summary(model)
        return .chart(model)
    }

    static func layerSpec(_ m: (name: String, def: [String: JSONValue]), encoding: [String: JSONValue], data: JSONValue?, transform: JSONValue?) throws -> LayerSpec {
        let name = m.name.lowercased()
        guard let mark = ChartMark(rawValue: name) else { throw Unsupported.mark(m.name, "the \(m.name) mark") }
        if let t = transform, t.array?.isEmpty == false { throw Unsupported.mark(m.name, "transforms") }
        return LayerSpec(mark: mark, markName: name, markDef: m.def, encoding: encoding, data: data)
    }

    static func title(_ v: JSONValue?) -> String? {
        if let s = v?.string { return s }
        if let s = v?["text"]?.string { return s }
        if let a = v?["text"]?.array { return a.compactMap(\.string).joined(separator: " ") }
        return nil
    }

    // MARK: - data

    enum Rows {
        case rows([[String: JSONValue]])
        case missing(String)
    }

    static func rows(_ data: JSONValue?, tables: ChartTables) throws -> Rows {
        guard let data else { return .rows([]) }
        if let values = data["values"] {
            guard let a = values.array else { return .missing("The chart's inline data isn't a list of rows.") }
            // [1, 2, 3] reads as rows of {"data": n}, as Vega-Lite does
            return .rows(a.map { $0.object ?? ["data": $0] })
        }
        if let ref = data["block"]?.string ?? data["table"]?.string {
            guard let t = tables.table(ref: ref) else {
                return .missing("Table \(ref.hasPrefix("^") ? ref : "^" + ref) isn't in this doc.")
            }
            return .rows(ChartCoercion.rows(t))
        }
        if data["url"] != nil {
            return .missing("This chart loads its data from a URL, which Taisce doesn't fetch.")
        }
        if let name = data["name"]?.string {
            return .missing("This chart reads a named dataset (\(name)), which Taisce can't supply.")
        }
        if data["sequence"] != nil || data["sphere"] != nil || data["graticule"] != nil {
            throw Unsupported.mark("chart", "generated data")
        }
        return .rows([])
    }

    struct Measure {
        var op: String
        var field: String?
        var label: String

        init(_ c: Channel) {
            op = c.aggregate ?? "none"
            field = c.field
            label = c.label
        }
    }

    struct Group {
        var dims: [String: JSONValue]
        var value: Double?
    }

    /// Rows grouped by `dims` (first appearance order), the measure folded per group.
    static func aggregate(_ data: [[String: JSONValue]], dims: [String], measure: Measure) throws -> [Group] {
        var order: [[JSONValue]] = []
        var buckets: [[JSONValue]: [Double]] = [:]
        var counts: [[JSONValue]: Int] = [:]
        for row in data {
            let key = dims.map { row[$0] ?? .null }
            if buckets[key] == nil {
                order.append(key)
                buckets[key] = []
                counts[key] = 0
            }
            counts[key, default: 0] += 1
            if let f = measure.field, let n = row[f]?.number { buckets[key, default: []].append(n) }
        }
        return order.map { key in
            let xs = buckets[key] ?? []
            let value: Double? = switch measure.op {
            case "count": Double(counts[key] ?? 0)
            case "sum": xs.reduce(0, +)
            case "mean", "average": xs.isEmpty ? nil : xs.reduce(0, +) / Double(xs.count)
            case "min": xs.min()
            case "max": xs.max()
            case "median": median(xs)
            // no aggregate (arcs): one row per group, the sum is that row's value
            default: xs.isEmpty ? nil : xs.reduce(0, +)
            }
            return Group(dims: Dictionary(uniqueKeysWithValues: zip(dims, key)), value: value)
        }
    }

    static func median(_ xs: [Double]) -> Double? {
        guard !xs.isEmpty else { return nil }
        let s = xs.sorted()
        return s.count % 2 == 1 ? s[s.count / 2] : (s[s.count / 2 - 1] + s[s.count / 2]) / 2
    }

    static func text(_ v: JSONValue) -> String {
        switch v {
        case let .string(s): s
        case let .number(n): ChartValue.format(n)
        case let .bool(b): b ? "true" : "false"
        case .null: "null"
        default: "\(v)"
        }
    }

    // MARK: - types and values

    enum Kind { case number, date, text }

    static func kind(_ t: ChartFieldType) -> Kind {
        switch t {
        case .quantitative: .number
        case .temporal: .date
        case .nominal, .ordinal: .text
        }
    }

    /// The declared type, else inferred: aggregated or all numbers →
    /// quantitative, all ISO dates → temporal, otherwise nominal.
    static func fieldType(_ c: Channel, values: [JSONValue]) -> ChartFieldType {
        if let t = c.type { return t }
        if c.aggregate != nil { return .quantitative }
        if c.timeUnit { return .temporal }
        let present = values.filter { !$0.isNull }
        if present.isEmpty { return .quantitative }
        if present.allSatisfy({ if case .number = $0 { true } else { false } }) { return .quantitative }
        if present.allSatisfy({ $0.string.flatMap { ChartCoercion.date($0) } != nil }) { return .temporal }
        return .nominal
    }

    /// A raw value as the axis type wants it; nil drops the point. An
    /// absent channel passes through as a placeholder.
    static func coerce(_ v: JSONValue?, _ type: ChartFieldType?, present: Bool) -> ChartValue?? {
        guard present else { return .some(nil) }
        guard let v, !v.isNull, let type else { return nil }
        switch type {
        case .quantitative:
            return v.number.map { .some(.number($0)) }
        case .temporal:
            if let s = v.string { return ChartCoercion.date(s).map { .some(.date($0)) } }
            if let n = v.number {
                // a bare year, else epoch milliseconds (Vega's convention)
                if n.rounded() == n, (1000...9999).contains(n) { return ChartCoercion.date(String(Int(n))).map { .some(.date($0)) } }
                return .some(.date(Date(timeIntervalSince1970: n / 1000)))
            }
            return nil
        case .nominal, .ordinal:
            return .some(.text(text(v)))
        }
    }

    static func stacking(_ v: JSONValue?, mark: ChartMark, grouped: Bool) -> ChartStacking {
        guard mark == .bar || mark == .area, !grouped else { return .none }
        guard let v else { return .standard }
        if v.isNull || v.isFalse { return .none }
        switch v.string {
        case "normalize": return .normalized
        case "center": return .center
        default: return .standard
        }
    }

    static func interpolation(_ s: String?) -> ChartInterpolation {
        switch s {
        case "monotone": .monotone
        case "step", "step-after", "step-before": .step
        case "basis", "cardinal", "catmull-rom", "natural": .catmullRom
        default: .linear
        }
    }

    /// The category order Vega-Lite would draw: ascending by default,
    /// `null` = data order, "descending", an explicit list, "-y"/"y" (or
    /// `{op, order}`) by the measure summed per category.
    static func order(_ sort: JSONValue?, values: [ChartValue], measures: [ChartValue?], measureChannel: String) -> [String] {
        var seen: [String] = []
        var total: [String: Double] = [:]
        for (i, v) in values.enumerated() {
            let k = v.text
            if !seen.contains(k) { seen.append(k) }
            if i < measures.count, let n = measures[i]?.number { total[k, default: 0] += n }
        }
        // ChartValue orders text numerically when both sides read as numbers
        let ascending = seen.sorted { ChartValue.text($0) < ChartValue.text($1) }
        guard let sort else { return ascending }
        if sort.isNull { return seen }
        if let explicit = sort.array {
            let listed = explicit.map(text).filter(seen.contains)
            return listed + seen.filter { !listed.contains($0) }
        }
        func byMeasure(descending: Bool) -> [String] {
            seen.sorted { a, b in
                let x = total[a] ?? 0, y = total[b] ?? 0
                return descending ? x > y : x < y
            }
        }
        if let s = sort.string {
            switch s {
            case "descending": return ascending.reversed()
            case "-\(measureChannel)", "-x", "-y": return byMeasure(descending: true)
            case measureChannel, "x", "y": return byMeasure(descending: false)
            default: return ascending
            }
        }
        if let o = sort.object {
            let descending = o["order"]?.string == "descending"
            if o["field"] != nil || o["op"] != nil || o["encoding"] != nil { return byMeasure(descending: descending) }
            return descending ? ascending.reversed() : ascending
        }
        return ascending
    }

    static func summary(_ m: ChartModel) -> String {
        let kinds = m.layers.map(\.mark.rawValue)
        let name: String = switch kinds.first {
        case "arc": (m.layers.first?.innerRadius ?? 0) > 0 ? "Donut chart" : "Pie chart"
        case "bar": "Bar chart"
        case "line": kinds.contains("point") ? "Line chart with points" : "Line chart"
        case "area": "Area chart"
        case "point": "Scatter plot"
        case "rule": "Rule chart"
        case "tick": "Strip plot"
        default: "Chart"
        }
        var parts = [m.title.map { "\(name), \($0)" } ?? name]
        if let x = m.x, let y = m.y, !x.title.isEmpty, !y.title.isEmpty { parts.append("\(y.title) by \(x.title)") }
        let n = m.layers.first?.points.count ?? 0
        parts.append("\(n) value\(n == 1 ? "" : "s")")
        if m.series.count > 1 { parts.append("\(m.series.count) series") }
        return parts.joined(separator: ", ")
    }
}
