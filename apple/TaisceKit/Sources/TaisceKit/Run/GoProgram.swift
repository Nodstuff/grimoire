import Foundation

/// A Go block turned into a `main.go` that builds: pure, no toolchain.
///
/// Shapes (`classify`):
/// - `.program`: it has `func main()` (and normally `package main`): run as
///   written (a missing or other package clause becomes `package main`);
/// - `.declarations`: only top-level funcs/types/vars/consts/imports: add
///   `package main` and a `main` that prints the try line (`fmt.Println(<expr>)`,
///   so a multi-value call prints space-separated), or an empty `main`;
/// - `.statements`: anything else: the statements go inside `func main()`,
///   with any func/type declarations among them hoisted to the top level.
///
/// Imports: the block's own are kept; standard-library packages it uses by
/// a qualified name (`strings.Fields`, `heap.Push`) are added from a fixed
/// table, unless that name is declared in the block. `GoProgram.unusedImports`
/// reads the compiler's "imported and not used" lines so the runner can drop
/// those and build again (wrapped shapes only).
public enum GoProgram {
    public enum Shape: Sendable, Hashable { case program, declarations, statements }

    public struct Prepared: Sendable, Hashable {
        public var shape: Shape
        /// the generated main.go
        public var source: String
        /// import paths added from the table (not written in the block)
        public var inferredImports: [String]
    }

    /// Standard-library packages by the name code refers to them with.
    public static let stdlib: [String: String] = [
        "fmt": "fmt", "strings": "strings", "sort": "sort", "strconv": "strconv",
        "math": "math", "errors": "errors", "time": "time", "os": "os",
        "heap": "container/heap", "list": "container/list", "ring": "container/ring",
        "unicode": "unicode", "utf8": "unicode/utf8", "utf16": "unicode/utf16",
        "bytes": "bytes", "slices": "slices", "maps": "maps", "sync": "sync",
        "atomic": "sync/atomic", "rand": "math/rand", "bits": "math/bits",
        "big": "math/big", "cmplx": "math/cmplx", "cmp": "cmp", "iter": "iter",
        "regexp": "regexp", "bufio": "bufio", "io": "io", "context": "context",
        "json": "encoding/json", "hex": "encoding/hex", "base64": "encoding/base64",
        "binary": "encoding/binary", "csv": "encoding/csv",
        "filepath": "path/filepath", "path": "path", "reflect": "reflect",
        "runtime": "runtime", "sha256": "crypto/sha256", "md5": "crypto/md5",
        "http": "net/http", "url": "net/url", "exec": "os/exec", "signal": "os/signal",
        "log": "log", "flag": "flag", "tabwriter": "text/tabwriter",
        "template": "text/template",
    ]

    // MARK: - entry points

    public static func classify(_ source: String) -> Shape {
        Parsed(source).shape
    }

    /// The `main.go` for `source`; `tryLine` is the expression a
    /// declarations block's `main` prints (blank = an empty `main`).
    public static func prepare(_ source: String, tryLine: String = "") -> Prepared {
        let p = Parsed(source)
        let expr = tryLine.trimmingCharacters(in: .whitespacesAndNewlines)
        var decls: [String] = []
        var mainBody: [String] = []
        switch p.shape {
        case .program:
            break
        case .declarations:
            decls = p.units.filter { $0.kind != .package && $0.kind != .imports }.map(\.text)
            if !expr.isEmpty { mainBody = ["fmt.Println(\(expr))"] }
        case .statements:
            for u in p.units {
                switch u.kind {
                case .package, .imports: continue
                case .funcOrType: decls.append(u.text)
                case .valueDecl, .statement: mainBody.append(u.text)
                }
            }
        }
        // what the generated code refers to (the try line included)
        var bodyForScan = p.skeletonWithoutImports
        if p.shape == .declarations, !expr.isEmpty { bodyForScan += "\nfmt.Println(" + Lexer.skeleton(expr) + ")" }
        let declared = declaredNames(in: bodyForScan)
        let have = Set(p.imports.map(\.name))
        let havePaths = Set(p.imports.map(\.path))
        var inferred: [String] = []
        for name in qualifiers(in: bodyForScan) where !have.contains(name) && !declared.contains(name) {
            guard let path = stdlib[name], !havePaths.contains(path), !inferred.contains(path) else { continue }
            inferred.append(path)
        }
        // the try line's Println needs fmt even when `fmt` is declared away
        inferred.sort()

        if p.shape == .program { return Prepared(shape: .program, source: asWritten(source, adding: inferred), inferredImports: inferred) }

        var out = "package main\n"
        let specs = p.imports.map(\.spec) + inferred.map { "\"\($0)\"" }
        if !specs.isEmpty {
            out += "\nimport (\n" + specs.map { "\t\($0)\n" }.joined() + ")\n"
        }
        for d in decls { out += "\n" + d.trimmingTrailingNewlines() + "\n" }
        if p.shape != .program {
            if mainBody.isEmpty {
                out += "\nfunc main() {}\n"
            } else {
                let body = mainBody.joined(separator: "\n").split(separator: "\n", omittingEmptySubsequences: false)
                    .map { $0.isEmpty ? "" : "\t" + $0 }.joined(separator: "\n")
                out += "\nfunc main() {\n" + body.trimmingTrailingNewlines() + "\n}\n"
            }
        }
        return Prepared(shape: p.shape, source: out, inferredImports: inferred)
    }

    /// A whole program as written: `package main` (added, or replacing
    /// another package clause) and any inferred imports in their own group
    /// right after it.
    static func asWritten(_ source: String, adding imports: [String]) -> String {
        var lines = source.replacingOccurrences(of: "\r\n", with: "\n").components(separatedBy: "\n")
        let skel = Lexer.skeleton(lines.joined(separator: "\n")).components(separatedBy: "\n")
        let group = imports.isEmpty ? [] : ["", "import ("] + imports.map { "\t\"\($0)\"" } + [")"]
        if let i = skel.firstIndex(where: { $0.trimmingCharacters(in: .whitespaces).hasPrefix("package ") }) {
            lines[i] = "package main"
            lines.insert(contentsOf: group, at: i + 1)
        } else {
            lines.insert(contentsOf: ["package main"] + group + [""], at: 0)
        }
        return lines.joined(separator: "\n")
    }

    /// The first top-level func's name (not a method, not `main`): the try
    /// line's hint, e.g. `pairSum(…)`.
    public static func firstFunction(in source: String) -> String? {
        for u in Parsed(source).units where u.kind == .funcOrType {
            let t = u.skeleton.trimmingCharacters(in: .whitespacesAndNewlines)
            guard t.hasPrefix("func "), !t.hasPrefix("func (") else { continue }
            let name = t.dropFirst(5).prefix { $0 == "_" || $0.isLetter || $0.isNumber }
            if !name.isEmpty, name != "main" { return String(name) }
        }
        return nil
    }

    /// Import paths the compiler says are unused (`"os" imported and not
    /// used`, `"math/rand" imported as r and not used`).
    public static func unusedImports(fromBuildOutput s: String) -> [String] {
        var out: [String] = []
        for line in s.split(separator: "\n") {
            guard line.contains("imported"), line.contains("and not used"),
                  let q1 = line.firstIndex(of: "\""),
                  let q2 = line[line.index(after: q1)...].firstIndex(of: "\"")
            else { continue }
            let path = String(line[line.index(after: q1)..<q2])
            if !out.contains(path) { out.append(path) }
        }
        return out
    }

    /// `source` with the import of each of `paths` removed (a single
    /// `import "x"` line or a spec inside an `import ( … )` group).
    public static func removingImports(_ paths: [String], from source: String) -> String {
        guard !paths.isEmpty else { return source }
        let quoted = Set(paths.map { "\"\($0)\"" })
        var lines = source.components(separatedBy: "\n")
        lines.removeAll { line in
            let t = line.trimmingCharacters(in: .whitespaces)
            let spec = t.hasPrefix("import ") ? String(t.dropFirst(7)).trimmingCharacters(in: .whitespaces) : t
            guard let last = spec.split(separator: " ").last else { return false }
            return quoted.contains(String(last)) && !t.hasSuffix("(")
        }
        return lines.joined(separator: "\n")
    }

    /// A minimal go.mod: `module run` at the toolchain's version
    /// (`go env GOVERSION` → `go1.26.1`).
    public static func goMod(goVersion: String) -> String {
        var v = goVersion.trimmingCharacters(in: .whitespacesAndNewlines)
        if v.hasPrefix("go") { v.removeFirst(2) }
        // a dev toolchain ("devel go1.27-abc …") can't be a go line
        let ok = !v.isEmpty && v.allSatisfy { $0.isNumber || $0 == "." }
        return "module run\n\ngo \(ok ? v : "1.22")\n"
    }

    // MARK: - scanning

    /// Package names used as `name.Member` (not `x.name.Member`).
    static func qualifiers(in skeleton: String) -> [String] {
        var out: [String] = []
        let chars = Array(skeleton.unicodeScalars)
        var i = 0
        while i < chars.count {
            let c = chars[i]
            guard isIdentStart(c) else { i += 1; continue }
            var j = i
            while j < chars.count, isIdent(chars[j]) { j += 1 }
            let prev = i > 0 ? chars[i - 1] : " "
            if j < chars.count, chars[j] == ".", prev != ".", j + 1 < chars.count, isIdentStart(chars[j + 1]) {
                let name = String(String.UnicodeScalarView(chars[i..<j]))
                if !out.contains(name) { out.append(name) }
            }
            i = j
        }
        return out
    }

    /// Names the block declares (`x :=`, `a, b :=`, `var`/`const`/`type`,
    /// func names, parameters, receivers, range variables): a package name
    /// shadowed by one of these is never imported.
    static func declaredNames(in skeleton: String) -> Set<String> {
        var names = Set<String>()
        let s = skeleton
        // short variable declarations: the identifiers before `:=`
        var rest = Substring(s)
        while let r = rest.range(of: ":=") {
            let before = rest[..<r.lowerBound]
            let stop = before.lastIndex { ";{(\n".contains($0) }
            let lhs = stop.map { before[before.index(after: $0)...] } ?? before
            for part in lhs.split(separator: ",") {
                var word = part.trimmingCharacters(in: .whitespaces)
                for kw in ["for ", "if ", "switch ", "range "] where word.hasPrefix(kw) { word = String(word.dropFirst(kw.count)) }
                if let w = word.split(separator: " ").last, isIdentifier(String(w)) { names.insert(String(w)) }
            }
            rest = rest[r.upperBound...]
        }
        let words = tokens(s)
        for (k, w) in words.enumerated() where ["var", "const", "type", "func"].contains(w) {
            if k + 1 < words.count, isIdentifier(words[k + 1]) { names.insert(words[k + 1]) }
        }
        // grouped `var ( a = 1 \n b int )` and parameter lists
        for group in parenGroups(after: ["var", "const", "type"], in: s) {
            for line in group.split(separator: "\n") {
                if let w = line.split(separator: " ").first.map(String.init), isIdentifier(w) { names.insert(w) }
            }
        }
        for params in funcParamLists(in: s) {
            for part in params.split(separator: ",") {
                if let w = part.split(whereSeparator: { $0 == " " || $0 == "\t" }).first.map(String.init), isIdentifier(w) {
                    names.insert(w)
                }
            }
        }
        return names
    }

    /// The parenthesised text after `var (`, `const (`, `type (`.
    static func parenGroups(after keywords: [String], in s: String) -> [String] {
        var out: [String] = []
        for kw in keywords {
            var rest = Substring(s)
            while let r = rest.range(of: kw + " (") ?? rest.range(of: kw + "(") {
                let open = rest.index(before: r.upperBound)
                if let close = matching(rest, from: open) {
                    out.append(String(rest[rest.index(after: open)..<close]))
                    rest = rest[close...]
                } else {
                    break
                }
            }
        }
        return out
    }

    /// Every `(…)` that is a func's receiver or parameter list.
    static func funcParamLists(in s: String) -> [String] {
        var out: [String] = []
        var rest = Substring(s)
        while let r = rest.range(of: "func") {
            let after = rest[r.upperBound...]
            // `func` as a whole word
            let prevOK = r.lowerBound == rest.startIndex || !isIdentChar(rest[rest.index(before: r.lowerBound)])
            guard prevOK, let first = after.first, first == " " || first == "(" else {
                rest = after
                continue
            }
            var cur = after
            // receiver, then (after an optional name) the parameters
            for _ in 0..<2 {
                let trimmed = cur.drop { $0 == " " }
                var probe = trimmed
                while let c = probe.first, isIdentChar(c) { probe = probe.dropFirst() }
                guard probe.first == "(", let close = matching(probe, from: probe.startIndex) else { break }
                out.append(String(probe[probe.index(after: probe.startIndex)..<close]))
                cur = probe[probe.index(after: close)...]
                // a receiver is followed by the method name and its params
                if trimmed.first != "(" { break }
            }
            rest = cur
        }
        return out
    }

    static func matching(_ s: Substring, from open: Substring.Index) -> Substring.Index? {
        var depth = 0
        var i = open
        while i < s.endIndex {
            switch s[i] {
            case "(": depth += 1
            case ")":
                depth -= 1
                if depth == 0 { return i }
            default: break
            }
            i = s.index(after: i)
        }
        return nil
    }

    static func tokens(_ s: String) -> [String] {
        var out: [String] = []
        var cur = ""
        for c in s {
            if isIdentChar(c) {
                cur.append(c)
            } else {
                if !cur.isEmpty { out.append(cur) }
                cur = ""
            }
        }
        if !cur.isEmpty { out.append(cur) }
        return out
    }

    static func isIdentChar(_ c: Character) -> Bool { c == "_" || c.isLetter || c.isNumber }
    static func isIdentStart(_ c: Unicode.Scalar) -> Bool { c == "_" || CharacterSet.letters.contains(c) }
    static func isIdent(_ c: Unicode.Scalar) -> Bool { isIdentStart(c) || CharacterSet.decimalDigits.contains(c) }
    static func isIdentifier(_ w: String) -> Bool {
        guard let f = w.unicodeScalars.first, isIdentStart(f) else { return false }
        return w.unicodeScalars.allSatisfy(isIdent) && !keywords.contains(w)
    }

    static let keywords: Set<String> = [
        "break", "case", "chan", "const", "continue", "default", "defer", "else", "fallthrough",
        "for", "func", "go", "goto", "if", "import", "interface", "map", "package", "range",
        "return", "select", "struct", "switch", "type", "var",
    ]

    // MARK: - parsing into top-level units

    struct Import: Hashable {
        /// the spec as written: `"fmt"` or `m "math"`
        var spec: String
        var path: String
        /// the name code uses: the alias, else the path's last element
        /// (`math/rand/v2` → `rand`)
        var name: String
    }

    struct Unit {
        enum Kind { case package, imports, funcOrType, valueDecl, statement }
        var kind: Kind
        var text: String
        var skeleton: String
    }

    struct Parsed {
        var units: [Unit] = []
        var imports: [Import] = []
        var shape: Shape = .statements
        var skeletonWithoutImports = ""

        init(_ source: String) {
            let src = source.replacingOccurrences(of: "\r\n", with: "\n")
            let lines = src.components(separatedBy: "\n")
            let skel = Lexer.skeleton(src).components(separatedBy: "\n")
            var depth = 0
            var current: (kind: Unit.Kind, text: [String], skel: [String])?
            // comment lines before a unit belong to it
            var leading: (text: [String], skel: [String]) = ([], [])
            func flush() {
                if let c = current { units.append(Unit(kind: c.kind, text: c.text.joined(separator: "\n"), skeleton: c.skel.joined(separator: "\n"))) }
                current = nil
            }
            for (i, line) in lines.enumerated() {
                let sk = i < skel.count ? skel[i] : ""
                let t = sk.trimmingCharacters(in: .whitespaces)
                if depth == 0 {
                    if t.isEmpty && current == nil {
                        if !line.trimmingCharacters(in: .whitespaces).isEmpty {
                            leading.text.append(line)
                            leading.skel.append(sk)
                        }
                        continue
                    }
                    let continues = t.hasPrefix(")") || t.hasPrefix("}") || t.hasPrefix(".") || t.isEmpty
                    if !continues || current == nil {
                        flush()
                        current = (Self.kind(of: t), leading.text, leading.skel)
                        leading = ([], [])
                    }
                }
                current?.text.append(line)
                current?.skel.append(sk)
                for c in sk {
                    if "({[".contains(c) { depth += 1 } else if ")}]".contains(c) { depth = max(0, depth - 1) }
                }
            }
            flush()
            // a unit is followed by blank lines it swallowed: trim them
            units = units.map { u in
                var u = u
                u.text = u.text.trimmingTrailingNewlines()
                return u
            }.filter { !$0.text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || $0.kind != .statement }
            imports = units.filter { $0.kind == .imports }.flatMap { Self.imports(in: $0.text) }
            let hasMain = units.contains { $0.kind == .funcOrType && Self.isMain($0.skeleton) }
            let onlyDecls = units.allSatisfy { $0.kind != .statement }
            shape = hasMain ? .program : (onlyDecls && units.contains { $0.kind == .funcOrType || $0.kind == .valueDecl } ? .declarations : .statements)
            skeletonWithoutImports = units.filter { $0.kind != .imports && $0.kind != .package }.map(\.skeleton).joined(separator: "\n")
        }

        static func kind(of t: String) -> Unit.Kind {
            func starts(_ kw: String) -> Bool {
                t == kw || t.hasPrefix(kw + " ") || t.hasPrefix(kw + "(") || t.hasPrefix(kw + "\t")
            }
            if starts("package") { return .package }
            if starts("import") { return .imports }
            if starts("type") { return .funcOrType }
            // a named func (or method); `func(…) {…}()` is a statement
            if t.hasPrefix("func ") || t.hasPrefix("func\t") { return .funcOrType }
            if t.hasPrefix("func (") {
                // method: `func (r T) Name(` vs a literal `func () {`
                return t.range(of: #"^func\s*\([^)]*\)\s*[A-Za-z_]\w*\s*[\[(]"#, options: .regularExpression) != nil ? .funcOrType : .statement
            }
            if starts("var") || starts("const") { return .valueDecl }
            return .statement
        }

        static func isMain(_ skeleton: String) -> Bool {
            skeleton.range(of: #"^\s*func\s+main\s*\(\s*\)"#, options: .regularExpression) != nil
        }

        static func imports(in text: String) -> [Import] {
            var specs: [String] = []
            let body = text.trimmingCharacters(in: .whitespacesAndNewlines).dropFirst("import".count).trimmingCharacters(in: .whitespacesAndNewlines)
            if body.hasPrefix("(") {
                let inner = body.dropFirst().dropLast(body.hasSuffix(")") ? 1 : 0)
                for line in inner.split(whereSeparator: { $0 == "\n" || $0 == ";" }) {
                    var l = String(line)
                    if let c = l.range(of: "//") { l = String(l[..<c.lowerBound]) }
                    l = l.trimmingCharacters(in: .whitespaces)
                    if !l.isEmpty { specs.append(l) }
                }
            } else {
                var l = body
                if let c = l.range(of: "//") { l = String(l[..<c.lowerBound]) }
                specs.append(l.trimmingCharacters(in: .whitespaces))
            }
            return specs.compactMap { spec in
                guard let q1 = spec.firstIndex(of: "\""), let q2 = spec[spec.index(after: q1)...].firstIndex(of: "\"") else { return nil }
                let path = String(spec[spec.index(after: q1)..<q2])
                let alias = spec[..<q1].trimmingCharacters(in: .whitespaces)
                var name = path.split(separator: "/").last.map(String.init) ?? path
                if name.count > 1, name.hasPrefix("v"), name.dropFirst().allSatisfy(\.isNumber) {
                    let parts = path.split(separator: "/")
                    if parts.count > 1 { name = String(parts[parts.count - 2]) }
                }
                return Import(spec: spec, path: path, name: alias.isEmpty ? name : alias)
            }
        }
    }

    /// Go source with comments, string and rune literals blanked to spaces
    /// (newlines kept), so scanning never matches inside them.
    enum Lexer {
        static func skeleton(_ s: String) -> String {
            var out = ""
            out.reserveCapacity(s.utf8.count)
            let chars = Array(s)
            var i = 0
            func blank(_ c: Character) -> Character { c == "\n" ? "\n" : " " }
            while i < chars.count {
                let c = chars[i]
                let next: Character? = i + 1 < chars.count ? chars[i + 1] : nil
                if c == "/", next == "/" {
                    while i < chars.count, chars[i] != "\n" { out.append(" "); i += 1 }
                } else if c == "/", next == "*" {
                    out.append("  ")
                    i += 2
                    while i < chars.count, !(chars[i] == "*" && i + 1 < chars.count && chars[i + 1] == "/") {
                        out.append(blank(chars[i]))
                        i += 1
                    }
                    if i < chars.count { out.append("  "); i += 2 }
                } else if c == "\"" || c == "'" {
                    out.append(c)
                    i += 1
                    while i < chars.count, chars[i] != c, chars[i] != "\n" {
                        if chars[i] == "\\", i + 1 < chars.count { out.append(" "); i += 1 }
                        out.append(" ")
                        i += 1
                    }
                    if i < chars.count, chars[i] == c { out.append(c); i += 1 }
                } else if c == "`" {
                    out.append(c)
                    i += 1
                    while i < chars.count, chars[i] != "`" { out.append(blank(chars[i])); i += 1 }
                    if i < chars.count { out.append("`"); i += 1 }
                } else {
                    out.append(c)
                    i += 1
                }
            }
            return out
        }
    }
}

extension String {
    func trimmingTrailingNewlines() -> String {
        var s = self
        while let last = s.last, last == "\n" || last == " " || last == "\t" { s.removeLast() }
        return s
    }
}
