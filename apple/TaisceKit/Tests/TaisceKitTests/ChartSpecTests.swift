import Foundation
import Testing
@testable import TaisceKit

private func chart(_ json: String, tables: ChartTables = ChartTables(), sourceLocation: SourceLocation = #_sourceLocation) -> ChartModel? {
    let r = ChartSpec.parse(json, tables: tables)
    guard case let .chart(m) = r else {
        Issue.record("expected a chart, got \(r)", sourceLocation: sourceLocation)
        return nil
    }
    return m
}

private func ys(_ l: ChartLayer) -> [Double] { l.points.compactMap { $0.y?.number } }
private func xs(_ l: ChartLayer) -> [String] { l.points.compactMap { $0.x?.text } }

/// The Vega-Lite corpus: each spec mapped to a ChartModel and checked.
@Suite struct ChartSpecTests {
    @Test func simpleBar() throws {
        let m = try #require(chart("""
        {"$schema": "https://vega.github.io/schema/vega-lite/v6.json",
         "title": "Fruit", "data": {"values": [{"a": "B", "b": 28}, {"a": "A", "b": 55}, {"a": "C", "b": 43}]},
         "mark": "bar", "encoding": {"x": {"field": "a", "type": "nominal", "axis": {"title": "Kind"}}, "y": {"field": "b", "type": "quantitative"}}}
        """))
        #expect(m.title == "Fruit")
        #expect(m.layers.map(\.mark) == [.bar])
        #expect(m.x == ChartAxis(title: "Kind", type: .nominal, domain: ["A", "B", "C"]))
        #expect(m.y?.title == "b" && m.y?.type == .quantitative)
        #expect(xs(m.layers[0]) == ["B", "A", "C"])
        #expect(ys(m.layers[0]) == [28, 55, 43])
        #expect(m.series.isEmpty)
        #expect(m.summary == "Bar chart, Fruit, b by Kind, 3 values")
        #expect(m.height == 260)
    }

    @Test func stackedBar() throws {
        let m = try #require(chart("""
        {"data": {"values": [{"m": "Jan", "k": "x", "v": 1}, {"m": "Jan", "k": "y", "v": 2}, {"m": "Feb", "k": "x", "v": 3}]},
         "mark": {"type": "bar"}, "encoding": {"x": {"field": "m", "type": "ordinal", "sort": null}, "y": {"field": "v", "type": "quantitative", "stack": "normalize"}, "color": {"field": "k", "type": "nominal", "title": "Kind"}}}
        """))
        #expect(m.layers[0].stacking == .normalized)
        #expect(!m.layers[0].grouped)
        #expect(m.series == ["x", "y"])
        #expect(m.colorTitle == "Kind")
        #expect(m.x?.domain == ["Jan", "Feb"], "sort: null keeps data order")
        #expect(m.layers[0].points.map(\.series) == ["x", "y", "x"])
    }

    @Test func groupedBar() throws {
        let m = try #require(chart("""
        {"data": {"values": [{"c": "a", "g": "one", "v": 1}, {"c": "a", "g": "two", "v": 2}]},
         "mark": "bar", "encoding": {"x": {"field": "c"}, "xOffset": {"field": "g"}, "y": {"field": "v", "type": "quantitative"}, "color": {"field": "g"}}}
        """))
        #expect(m.layers[0].grouped)
        #expect(m.layers[0].stacking == .none)
        #expect(m.x?.type == .nominal, "strings infer nominal")
        #expect(m.series == ["one", "two"])
    }

    @Test func horizontalBarSortedByMeasure() throws {
        let m = try #require(chart("""
        {"data": {"values": [{"n": "a", "v": 2}, {"n": "b", "v": 9}, {"n": "c", "v": 5}]},
         "mark": "bar", "encoding": {"y": {"field": "n", "type": "nominal", "sort": "-x"}, "x": {"field": "v", "type": "quantitative"}}}
        """))
        #expect(m.y?.domain == ["b", "c", "a"])
        #expect(m.x?.type == .quantitative)
        #expect(m.layers[0].points.first?.x == .number(2))
    }

    @Test func line() throws {
        let m = try #require(chart("""
        {"data": {"values": [{"x": 1, "y": 3}, {"x": 2, "y": 1}, {"x": 3, "y": 4}]},
         "mark": {"type": "line", "interpolate": "monotone"}, "encoding": {"x": {"field": "x"}, "y": {"field": "y"}}}
        """))
        #expect(m.x?.type == .quantitative && m.y?.type == .quantitative, "numbers infer quantitative")
        #expect(m.layers[0].interpolation == .monotone)
        #expect(m.layers[0].points.map(\.x) == [.number(1), .number(2), .number(3)])
        #expect(m.x?.domain == nil)
    }

    @Test func multiSeriesLine() throws {
        let m = try #require(chart("""
        {"data": {"values": [{"d": "2026-01-01", "s": "A", "v": 1}, {"d": "2026-02-01", "s": "A", "v": 2}, {"d": "2026-01-01", "s": "B", "v": 3}]},
         "mark": "line", "encoding": {"x": {"field": "d", "type": "temporal"}, "y": {"field": "v", "type": "quantitative"}, "color": {"field": "s", "type": "nominal"}}}
        """))
        #expect(m.series == ["A", "B"])
        #expect(m.layers[0].colored)
        #expect(m.summary.hasSuffix("2 series"))
    }

    @Test func area() throws {
        let m = try #require(chart("""
        {"data": {"values": [{"t": 1, "v": 1, "k": "a"}, {"t": 1, "v": 2, "k": "b"}]},
         "mark": "area", "encoding": {"x": {"field": "t", "type": "quantitative"}, "y": {"field": "v", "type": "quantitative", "stack": "center"}, "color": {"field": "k"}}}
        """))
        #expect(m.layers[0].mark == .area)
        #expect(m.layers[0].stacking == .center)
    }

    @Test func scatterWithSize() throws {
        let m = try #require(chart("""
        {"data": {"values": [{"a": 1, "b": 2, "s": 10}, {"a": 2, "b": 3, "s": 20}, {"a": 3, "b": 1, "s": 30}]},
         "mark": {"type": "point", "filled": false}, "encoding": {"x": {"field": "a", "type": "quantitative"}, "y": {"field": "b", "type": "quantitative"}, "size": {"field": "s", "type": "quantitative"}}}
        """))
        #expect(m.layers[0].points.compactMap(\.size) == [20, 160, 300])
        #expect(!m.layers[0].filled)
        #expect(m.summary.hasPrefix("Scatter plot"))
    }

    @Test func pie() throws {
        let m = try #require(chart("""
        {"data": {"values": [{"c": "a", "v": 4}, {"c": "b", "v": 6}]},
         "mark": "arc", "encoding": {"theta": {"field": "v", "type": "quantitative"}, "color": {"field": "c", "type": "nominal"}}}
        """))
        #expect(m.isArc)
        #expect(m.layers[0].innerRadius == 0)
        #expect(m.layers[0].points.map(\.series) == ["a", "b"])
        #expect(ys(m.layers[0]) == [4, 6])
        #expect(m.x == nil && m.y == nil)
        #expect(m.summary.hasPrefix("Pie chart"))
    }

    @Test func donutWithCount() throws {
        let m = try #require(chart("""
        {"data": {"values": [{"c": "a"}, {"c": "b"}, {"c": "a"}]},
         "mark": {"type": "arc", "innerRadius": 60, "outerRadius": 120}, "encoding": {"theta": {"aggregate": "count"}, "color": {"field": "c"}}}
        """))
        #expect(m.layers[0].innerRadius == 0.5)
        #expect(ys(m.layers[0]) == [2, 1])
        #expect(m.summary.hasPrefix("Donut chart"))
    }

    @Test func layeredLinePoint() throws {
        let m = try #require(chart("""
        {"data": {"values": [{"x": "2026-10-01", "y": 1}, {"x": "2026-10-02", "y": 2}]},
         "encoding": {"x": {"field": "x", "type": "temporal"}, "y": {"field": "y", "type": "quantitative"}},
         "layer": [{"mark": "line"}, {"mark": {"type": "point", "filled": true}}]}
        """))
        #expect(m.layers.map(\.mark) == [.line, .point])
        #expect(m.layers[1].points.count == 2)
        #expect(m.summary.hasPrefix("Line chart with points"))
    }

    @Test func linePointTrueAddsDots() throws {
        let m = try #require(chart("""
        {"data": {"values": [{"x": 1, "y": 1}]}, "mark": {"type": "line", "point": true}, "encoding": {"x": {"field": "x"}, "y": {"field": "y"}}}
        """))
        #expect(m.layers.map(\.mark) == [.line, .point])
    }

    @Test func barWithMeanRule() throws {
        let m = try #require(chart("""
        {"data": {"values": [{"c": "a", "v": 2}, {"c": "b", "v": 4}]},
         "layer": [{"mark": "bar", "encoding": {"x": {"field": "c"}, "y": {"field": "v", "type": "quantitative"}}},
                   {"mark": "rule", "encoding": {"y": {"field": "v", "aggregate": "mean"}}}]}
        """))
        #expect(m.layers.map(\.mark) == [.bar, .rule])
        #expect(m.layers[1].points == [ChartPoint(x: nil, y: .number(3), series: nil)])
    }

    @Test func ruleAtDatum() throws {
        let m = try #require(chart(#"{"mark": "rule", "encoding": {"y": {"datum": 50}}}"#))
        #expect(m.layers[0].points.first?.y == .number(50))
    }

    @Test func tick() throws {
        let m = try #require(chart("""
        {"data": {"values": [{"k": "a", "v": 1}, {"k": "a", "v": 2}]}, "mark": "tick", "encoding": {"x": {"field": "v"}, "y": {"field": "k"}}}
        """))
        #expect(m.layers[0].mark == .tick)
        #expect(m.y?.type == .nominal)
        #expect(m.summary.hasPrefix("Strip plot"))
    }

    @Test func aggregateSumAndCountAndMean() throws {
        let rows = #"[{"c": "a", "v": 1}, {"c": "b", "v": 5}, {"c": "a", "v": 3}]"#
        func spec(_ agg: String) -> String {
            #"{"data": {"values": \#(rows)}, "mark": "bar", "encoding": {"x": {"field": "c"}, "y": {"field": "v", "aggregate": "\#(agg)"}}}"#
        }
        let sum = try #require(chart(spec("sum")))
        #expect(ys(sum.layers[0]) == [4, 5])
        #expect(sum.y?.title == "Sum of v" && sum.y?.type == .quantitative)
        let mean = try #require(chart(spec("mean")))
        #expect(ys(mean.layers[0]) == [2, 5])
        let count = try #require(chart(#"{"data": {"values": \#(rows)}, "mark": "bar", "encoding": {"x": {"field": "c"}, "y": {"aggregate": "count"}}}"#))
        #expect(ys(count.layers[0]) == [2, 1])
        #expect(count.y?.title == "Count")
    }

    @Test func sortVariants() throws {
        let rows = #"[{"c": "b", "v": 1}, {"c": "a", "v": 3}, {"c": "c", "v": 2}]"#
        func domain(_ sort: String) throws -> [String]? {
            try #require(chart(#"{"data": {"values": \#(rows)}, "mark": "bar", "encoding": {"x": {"field": "c", "sort": \#(sort)}, "y": {"field": "v"}}}"#)).x?.domain
        }
        #expect(try domain(#""ascending""#) == ["a", "b", "c"])
        #expect(try domain(#""descending""#) == ["c", "b", "a"])
        #expect(try domain(#""-y""#) == ["a", "c", "b"])
        #expect(try domain(#""y""#) == ["b", "c", "a"])
        #expect(try domain(#"["c", "a"]"#) == ["c", "a", "b"])
        #expect(try domain(#"{"op": "sum", "field": "v", "order": "descending"}"#) == ["a", "c", "b"])
        #expect(try domain("null") == ["b", "a", "c"])
    }

    @Test func numericCategoriesSortNumerically() throws {
        let m = try #require(chart(#"{"data": {"values": [{"k": 10, "v": 1}, {"k": 9, "v": 1}]}, "mark": "bar", "encoding": {"x": {"field": "k", "type": "ordinal"}, "y": {"field": "v"}}}"#))
        #expect(m.x?.domain == ["9", "10"])
    }

    @Test func temporalAxisScrollsWhenWide() throws {
        var cal = Calendar(identifier: .gregorian)
        cal.timeZone = .current
        let values = (0..<40).map { #"{"d": "2026-01-\#(String(format: "%02d", $0 % 20 + 1))T00:00:00Z", "v": \#($0)}"# }
        let m = try #require(chart(#"{"data": {"values": [\#(values.joined(separator: ","))]}, "mark": "line", "encoding": {"x": {"field": "d", "type": "temporal"}, "y": {"field": "v"}}}"#))
        #expect(m.x?.type == .temporal)
        #expect(m.layers[0].points.allSatisfy { $0.x?.date != nil })
        #expect(m.visibleXCount == nil, "20 distinct days only")

        let wide = (0..<30).map { #"{"d": "2026-01-\#(String(format: "%02d", $0 + 1))", "v": \#($0)}"# }
        let w = try #require(chart(#"{"data": {"values": [\#(wide.joined(separator: ","))]}, "mark": "bar", "encoding": {"x": {"field": "d"}, "y": {"field": "v"}}}"#))
        #expect(w.x?.type == .temporal, "ISO strings infer temporal")
        #expect(w.visibleXCount == 16)
        let first = try #require(w.layers[0].points.first?.x?.date)
        #expect(cal.dateComponents([.year, .month, .day, .hour], from: first) == DateComponents(year: 2026, month: 1, day: 1, hour: 0), "date-only is local midnight")
    }

    @Test func temporalNumbers() throws {
        let m = try #require(chart(#"{"data": {"values": [{"y": 2024, "v": 1}, {"y": 2025, "v": 2}]}, "mark": "line", "encoding": {"x": {"field": "y", "type": "temporal"}, "y": {"field": "v"}}}"#))
        let d = try #require(m.layers[0].points.first?.x?.date)
        #expect(Calendar.current.component(.year, from: d) == 2024)
        #expect(ChartCoercion.date("2026-10-01T14:00Z") == Date(timeIntervalSince1970: 1_790_863_200))
        #expect(ChartCoercion.date("2026-10-01T15:00:00+01:00") == Date(timeIntervalSince1970: 1_790_863_200))
        #expect(ChartCoercion.date("2026-13-01") == nil)
        #expect(ChartCoercion.date("October") == nil)
    }

    @Test func tableBacked() throws {
        let table = RenderNode.Table(header: ["**Month**", "Sales", "Region"], rows: [["2026-01-01", "1,200", "EU"], ["2026-02-01", "950", "US"], ["2026-03-01", "", "EU"]], alignments: [])
        let tables = ChartTables(blocks: [
            (id: "01a080ba-17b1-7a43-93d5-000000000001", nodes: [.paragraph(inline: "hi")]),
            (id: "01a080ba-17b1-7a43-93d5-4d99f46f2abc", nodes: [.heading(level: 2, inline: "Data"), .table(table)]),
        ])
        #expect(tables.table(ref: "^6f2abc") == table)
        #expect(tables.table(ref: "6F2ABC") == table)
        #expect(tables.table(ref: "01a080ba-17b1-7a43-93d5-4d99f46f2abc") == table)
        #expect(tables.table(ref: "^000001") == nil, "that block has no table")
        let spec = #"{"data": {"block": "^6f2abc"}, "mark": "bar", "encoding": {"x": {"field": "Month", "type": "temporal"}, "y": {"field": "Sales", "type": "quantitative"}, "color": {"field": "Region"}}}"#
        let m = try #require(chart(spec, tables: tables))
        #expect(ys(m.layers[0]) == [1200, 950], "the empty cell is dropped")
        #expect(m.series == ["EU", "US"])
        // editing the table updates the chart
        var edited = table
        edited.rows[1][1] = "2000"
        let m2 = try #require(chart(spec.replacingOccurrences(of: "block", with: "table"), tables: ChartTables(blocks: [(id: "01a080ba-17b1-7a43-93d5-4d99f46f2abc", nodes: [.table(edited)])])))
        #expect(ys(m2.layers[0]) == [1200, 2000])
    }

    @Test func missingTableAndURLData() {
        #expect(ChartSpec.parse(#"{"data": {"block": "^ffffff"}, "mark": "bar"}"#) == .noData(mark: "bar", note: "Table ^ffffff isn't in this doc."))
        #expect(ChartSpec.parse(#"{"data": {"url": "data/cars.json"}, "mark": "point", "encoding": {"x": {"field": "a"}}}"#)
            == .noData(mark: "point", note: "This chart loads its data from a URL, which Taisce doesn't fetch."))
    }

    @Test func invalidJSONReportsTheLine() {
        let r = ChartSpec.parse("{\n  \"mark\": \"bar\",\n  \"data\": oops\n}")
        guard case let .invalid(message, line, excerpt) = r else {
            Issue.record("expected invalid, got \(r)")
            return
        }
        #expect(line == 3)
        #expect(excerpt == "\"data\": oops")
        #expect(!message.isEmpty && !message.contains("around line"))
        #expect(ChartSpec.parse("[1, 2]") == .invalid(message: "A Vega-Lite spec is a JSON object.", line: 1, excerpt: "[1, 2]"))
    }

    @Test func unsupported() {
        #expect(ChartSpec.parse(#"{"mark": "rect", "encoding": {"x": {"field": "a"}}}"#) == .unsupported(mark: "rect", reason: "the rect mark"))
        #expect(ChartSpec.parse(#"{"mark": {"type": "geoshape"}}"#) == .unsupported(mark: "geoshape", reason: "the geoshape mark"))
        #expect(ChartSpec.parse(#"{"hconcat": [{"mark": "bar"}]}"#) == .unsupported(mark: "hconcat", reason: "concatenated views"))
        #expect(ChartSpec.parse(#"{"facet": {"row": {"field": "a"}}, "spec": {"mark": "bar"}}"#) == .unsupported(mark: "facet", reason: "faceted views"))
        #expect(ChartSpec.parse(#"{"mark": "bar", "transform": [{"filter": "datum.a > 1"}], "encoding": {"x": {"field": "a"}}}"#) == .unsupported(mark: "bar", reason: "transforms"))
        #expect(ChartSpec.parse(#"{"mark": "bar", "encoding": {"x": {"field": "a"}, "row": {"field": "b"}}}"#) == .unsupported(mark: "bar", reason: "faceted views"))
        #expect(ChartSpec.parse(#"{"mark": "bar", "encoding": {"y": {"field": "a", "aggregate": "variance"}}}"#) == .unsupported(mark: "bar", reason: "the variance aggregate"))
        #expect(ChartSpec.parse(#"{"data": {"values": []}}"#) == .unsupported(mark: "chart", reason: "a spec without a mark"))
    }

    @Test func heightIsClamped() throws {
        let m = try #require(chart(#"{"height": 900, "mark": "rule", "encoding": {"y": {"datum": 1}}}"#))
        #expect(m.height == 320)
    }

    @Test func coercion() {
        #expect(ChartCoercion.number("1,234.5") == 1234.5)
        #expect(ChartCoercion.number("12%") == 12)
        #expect(ChartCoercion.number("$5") == 5)
        #expect(ChartCoercion.number("inf") == nil)
        #expect(ChartCoercion.number("abc") == nil)
        #expect(ChartCoercion.plain("**Bold** `code` [[Doc|alias]]") == "Bold code alias")
    }
}
