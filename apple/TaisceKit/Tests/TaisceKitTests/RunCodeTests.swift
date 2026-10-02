import Foundation
import Testing
@testable import TaisceKit

@Suite struct FenceInfoTests {
    @Test func languageAndAttributes() {
        let f = FenceInfo("bash cwd=~/code/portus")
        #expect(f.language == "bash")
        #expect(f.attributes == ["cwd": "~/code/portus"])
        #expect(f.cwd(home: "/Users/t") == "/Users/t/code/portus")
        #expect(FenceInfo("Go").normalizedLanguage == "go")
        #expect(FenceInfo(nil).language == nil)
        #expect(FenceInfo("").language == nil)
    }

    @Test func quotedValuesAndOddWords() {
        let f = FenceInfo(#"zsh  cwd="~/My Code"  title='a b' flag =x CWD=/tmp"#)
        #expect(f.language == "zsh")
        // later keys win; keys are case-insensitive; `=x` (no key) is ignored
        #expect(f.attributes["cwd"] == "/tmp")
        #expect(f.attributes["title"] == "a b")
        #expect(f.attributes.count == 2)
        #expect(FenceInfo.expandTilde("~", home: "/h") == "/h")
        #expect(FenceInfo.expandTilde("~other/x", home: "/h") == "~other/x")
        #expect(FenceInfo("bash cwd=").cwd(home: "/h") == nil)
    }

    @Test func rendererSplitsTheInfoString() {
        let b = Block(id: "b", docID: "d", parentID: nil, orderKey: "i", blockType: .code, content: "```bash cwd=~/code/portus\nls\n```")
        #expect(BlockRenderer.render(b) == [.code(language: "bash", code: "ls", attributes: ["cwd": "~/code/portus"])])
        // a diagram fence with attributes is still a diagram
        let m = Block(id: "m", docID: "d", parentID: nil, orderKey: "i", blockType: .paragraph, content: "```mermaid theme=dark\ngraph TD\n```")
        #expect(BlockRenderer.render(m) == [.diagram(kind: "mermaid", source: "graph TD")])
    }

    @Test func runnableLanguages() {
        #expect(RunnableLanguage("bash") == .shell(interpreter: "/bin/bash"))
        #expect(RunnableLanguage("SH") == .shell(interpreter: "/bin/sh"))
        #expect(RunnableLanguage("zsh") == .shell(interpreter: "/bin/zsh"))
        #expect(RunnableLanguage("go") == .go)
        #expect(RunnableLanguage("python") == nil)
        #expect(RunnableLanguage(nil) == nil)
    }
}

@Suite struct FenceEditTests {
    @Test func replacesTheFenceBody() {
        let content = "```go\nfmt.Println(1)\n```"
        #expect(FenceEdit.replacingCode(in: content, with: "fmt.Println(2)\nfmt.Println(3)") == "```go\nfmt.Println(2)\nfmt.Println(3)\n```")
        // the info string and a tilde fence survive; a body holding ``` gets a longer fence
        #expect(FenceEdit.replacingCode(in: "~~~bash cwd=~/x\nls\n~~~", with: "pwd") == "~~~bash cwd=~/x\npwd\n~~~")
        #expect(FenceEdit.replacingCode(in: "```sh\necho\n```", with: "cat <<EOF\n```\nEOF") == "````sh\ncat <<EOF\n```\nEOF\n````")
        #expect(FenceEdit.replacingCode(in: "no fence here", with: "x") == nil)
    }
}

@Suite struct GoProgramTests {
    static let pairSum = """
    func pairSum(nums []int, target int) (int, int, bool) {
    \tl, r := 0, len(nums)-1
    \tfor l < r {
    \t\ts := nums[l] + nums[r]
    \t\tswitch {
    \t\tcase s == target:
    \t\t\treturn l, r, true
    \t\tcase s < target:
    \t\t\tl++
    \t\tdefault:
    \t\t\tr--
    \t\t}
    \t}
    \treturn -1, -1, false
    }
    """

    @Test func classifiesEachShape() {
        #expect(GoProgram.classify("package main\n\nimport \"fmt\"\n\nfunc main() {\n\tfmt.Println(1)\n}") == .program)
        #expect(GoProgram.classify("func main() { println(1) }") == .program)
        #expect(GoProgram.classify(Self.pairSum) == .declarations)
        #expect(GoProgram.classify("type P struct{ X int }\n\nvar origin = P{}\nconst n = 3") == .declarations)
        #expect(GoProgram.classify("x := 3\nfmt.Println(x * 2)") == .statements)
        #expect(GoProgram.classify("var total int\nfor i := 0; i < 3; i++ {\n\ttotal += i\n}\nfmt.Println(total)") == .statements)
        // a func literal call is a statement, a method a declaration
        #expect(GoProgram.classify("func() {\n\tprintln(1)\n}()") == .statements)
        #expect(GoProgram.classify("func (p P) Len() int { return 0 }") == .declarations)
        // comments and strings never decide anything
        #expect(GoProgram.classify("// func main() {}\ns := \"func main() {}\"\nprintln(s)") == .statements)
    }

    @Test func programRunsAsWritten() {
        let src = "package main\n\nfunc main() {\n\tfmt.Println(strings.ToUpper(\"a\"))\n}"
        let p = GoProgram.prepare(src)
        #expect(p.shape == .program)
        #expect(p.inferredImports == ["fmt", "strings"])
        #expect(p.source == "package main\n\nimport (\n\t\"fmt\"\n\t\"strings\"\n)\n\nfunc main() {\n\tfmt.Println(strings.ToUpper(\"a\"))\n}")
        // nothing to add: byte for byte
        let full = "package main\n\nimport \"fmt\"\n\nfunc main() { fmt.Println(1) }\n"
        #expect(GoProgram.prepare(full).source == full)
        #expect(GoProgram.prepare("func main() {}").source == "package main\n\nfunc main() {}")
    }

    @Test func declarationsGetAMainFromTheTryLine() {
        let p = GoProgram.prepare(Self.pairSum, tryLine: "pairSum([]int{1,2,4,7}, 6)")
        #expect(p.shape == .declarations)
        #expect(p.inferredImports == ["fmt"])
        #expect(p.source.hasPrefix("package main\n\nimport (\n\t\"fmt\"\n)\n\nfunc pairSum("))
        #expect(p.source.hasSuffix("\nfunc main() {\n\tfmt.Println(pairSum([]int{1,2,4,7}, 6))\n}\n"))
        // an empty try line: an empty main, so the block at least compiles
        let empty = GoProgram.prepare(Self.pairSum, tryLine: "  ")
        #expect(empty.inferredImports.isEmpty)
        #expect(empty.source.hasSuffix("\nfunc main() {}\n"))
    }

    @Test func statementsAreWrappedAndDeclarationsHoisted() {
        let src = "// sum a few\nnums := []int{3, 1, 2}\nsort.Ints(nums)\n\nfunc double(x int) int { return x * 2 }\n\nfmt.Println(double(nums[0]))"
        let p = GoProgram.prepare(src)
        #expect(p.shape == .statements)
        #expect(p.inferredImports == ["fmt", "sort"])
        #expect(p.source == """
        package main

        import (
        \t"fmt"
        \t"sort"
        )

        func double(x int) int { return x * 2 }

        func main() {
        \t// sum a few
        \tnums := []int{3, 1, 2}
        \tsort.Ints(nums)
        \tfmt.Println(double(nums[0]))
        }

        """)
    }

    @Test func importsFromTheTable() {
        let src = "h := &IntHeap{2, 1}\nheap.Init(h)\nb := strings.Builder{}\nb.WriteString(strconv.Itoa(utf8.RuneLen('é')))\n_ = math.Sqrt(2)\n_ = errors.New(\"x\")\n_ = time.Now()\n_ = os.Args\n_ = unicode.IsUpper('A')\n_ = bytes.Buffer{}\n_ = slices.Max([]int{1})\n_ = maps.Keys(map[int]int{})\nvar mu sync.Mutex\n_ = &mu"
        let p = GoProgram.prepare(src)
        #expect(Set(p.inferredImports) == ["container/heap", "strings", "strconv", "unicode/utf8", "math", "errors", "time", "os", "unicode", "bytes", "slices", "maps", "sync"])
        #expect(p.inferredImports == p.inferredImports.sorted())
    }

    @Test func shadowedNamesAndWrittenImportsAreNotImported() {
        // a variable named like a package; a field chain; a written import; a string
        let src = """
        import m "math"
        list := []int{1}
        url := struct{ Host string }{"h"}
        fmt.Println(list, url.Host, m.Pi, a.b.strings, "sort.Ints")
        """
        let p = GoProgram.prepare(src)
        #expect(p.inferredImports == ["fmt"])
        #expect(p.source.contains("\tm \"math\"\n"))
        // parameters and range variables shadow too
        let q = GoProgram.prepare("func f(path string, time int) string { return path }\nfor _, bytes := range []string{} { _ = bytes }")
        #expect(q.inferredImports.isEmpty)
    }

    @Test func unusedImportsFromTheCompiler() {
        let out = """
        # run
        ./main.go:4:2: "os" imported and not used
        ./main.go:5:2: "math/rand" imported as r and not used
        ./main.go:9:2: declared and not used: x
        """
        #expect(GoProgram.unusedImports(fromBuildOutput: out) == ["os", "math/rand"])
        let src = "package main\n\nimport (\n\t\"fmt\"\n\t\"os\"\n\tr \"math/rand\"\n)\n\nimport \"os\"\n"
        #expect(GoProgram.removingImports(["os", "math/rand"], from: src) == "package main\n\nimport (\n\t\"fmt\"\n)\n\n")
    }

    @Test func goModFromTheToolchainVersion() {
        #expect(GoProgram.goMod(goVersion: "go1.26.1\n") == "module run\n\ngo 1.26.1\n")
        #expect(GoProgram.goMod(goVersion: "devel go1.27-abc") == "module run\n\ngo 1.22\n")
    }
}

@Suite struct LoginEnvironmentTests {
    @Test func parsesEnvZero() {
        var data = Data("Last login: today\nwelcome!\nPATH=/opt/homebrew/bin:/usr/bin".utf8)
        data.append(0)
        data.append(contentsOf: Array("MULTI=line one\nline two".utf8))
        data.append(0)
        data.append(contentsOf: Array("EMPTY=".utf8))
        data.append(0)
        data.append(contentsOf: Array("SHLVL=2".utf8))
        data.append(0)
        data.append(contentsOf: Array("_=/usr/bin/env".utf8))
        data.append(0)
        data.append(contentsOf: Array("not a record".utf8))
        data.append(0)
        data.append(contentsOf: Array("EQ=a=b".utf8))
        data.append(0)
        let env = LoginEnvironment.parse(data)
        #expect(env == ["PATH": "/opt/homebrew/bin:/usr/bin", "MULTI": "line one\nline two", "EMPTY": "", "EQ": "a=b"])
    }

    @Test func whichSearchesThePath() {
        let present: Set<String> = ["/opt/homebrew/bin/go"]
        #expect(LoginEnvironment.which("go", path: "/usr/bin::/opt/homebrew/bin", isExecutable: { present.contains($0) }) == "/opt/homebrew/bin/go")
        #expect(LoginEnvironment.which("go", path: "/usr/bin", isExecutable: { present.contains($0) }) == nil)
        #expect(LoginEnvironment.which("go", path: nil) == nil)
    }

    @Test func seedIsMinimal() {
        let s = LoginEnvironment.seed(home: "/Users/t", user: "t", shell: "/bin/zsh", tmpdir: nil)
        #expect(s["PATH"] == "/usr/bin:/bin:/usr/sbin:/sbin")
        #expect(Set(s.keys) == ["HOME", "USER", "LOGNAME", "SHELL", "PATH", "LANG"])
    }
}

@Suite struct RunOutputTests {
    @Test func stripsAnsi() {
        #expect(TerminalTextDecoder.strip("\u{1B}[1;31mred\u{1B}[0m plain") == "red plain")
        #expect(TerminalTextDecoder.strip("\u{1B}]0;title\u{07}after") == "after")
        #expect(TerminalTextDecoder.strip("\u{1B}]8;;http://x\u{1B}\\link\u{1B}]8;;\u{1B}\\") == "link")
        #expect(TerminalTextDecoder.strip("a\u{1B}(Bb\u{1B}7c\u{1B}[?25ld") == "abcd")
    }

    @Test func carriesSplitSequencesAndCharacters() {
        var d = TerminalTextDecoder()
        let bytes = Array("x\u{1B}[31mé€😀y".utf8)
        var out = ""
        for b in bytes { out += d.decode([b]) }
        out += d.decode([], final: true)
        #expect(out == "xé€😀y")
    }

    @Test func collectorCapsAndMerges() {
        var c = OutputCollector(cap: 10)
        c.add(Array("hello ".utf8), from: .stdout)
        c.add(Array("there".utf8), from: .stdout)
        c.add(Array("err".utf8), from: .stderr)
        #expect(c.truncated)
        #expect(c.bytes == 10)
        #expect(c.drain() == [OutputChunk(.stdout, "hello ther")])
        #expect(c.drain().isEmpty)
        c.add(Array("more".utf8), from: .stdout)
        #expect(c.drain().isEmpty)
    }

    @Test func interleavesStreamsInArrivalOrder() {
        var c = OutputCollector()
        c.add(Array("a".utf8), from: .stdout)
        c.add(Array("b".utf8), from: .stderr)
        c.add(Array("c".utf8), from: .stdout)
        c.add(Array("d".utf8), from: .stdout)
        #expect(c.drain() == [OutputChunk(.stdout, "a"), OutputChunk(.stderr, "b"), OutputChunk(.stdout, "cd")])
        var log = OutputLog([OutputChunk(.stdout, "1")])
        log.append([OutputChunk(.stdout, "2"), OutputChunk(.stderr, "")])
        #expect(log.chunks == [OutputChunk(.stdout, "12")])
    }

    @Test func tailCutsAtALine() {
        let log = OutputLog([OutputChunk(.stdout, "one\ntwo\n"), OutputChunk(.stderr, "three\n")])
        let t = log.tail(9)
        #expect(t.dropped)
        #expect(t.chunks == [OutputChunk(.stderr, "three\n")])
        #expect(log.tail(12).chunks == [OutputChunk(.stdout, "two\n"), OutputChunk(.stderr, "three\n")])
        #expect(log.tail(100).dropped == false)
    }
}

@Suite struct RunTrustTests {
    let me = RunTrust.Me(principalID: "p-tom", name: "Tom")

    func op(_ by: String, _ name: String, epoch: Int, block: String?, type: String = "replace", applied: Bool = true) -> DocHistoryEntry {
        DocHistoryEntry(opID: "op-\(epoch)", principalName: name, principalKind: by == "p-tom" ? "human" : "agent", applied: applied,
                        principalID: by, epoch: epoch, targetBlock: block, opType: type)
    }

    @Test func yourOwnBlockRuns() {
        let h = [op("p-claude", "claude:x", epoch: 5, block: "other"), op("p-tom", "Tom", epoch: 4, block: "b1", type: "insert")]
        #expect(RunTrust.decide(block: "b1", history: h, me: me, practiceEditedByMe: false, approval: nil, docEpoch: 5) == .run)
    }

    @Test func someoneElsesBlockAsks() {
        let h = [op("p-claude", "claude:tagger", epoch: 6, block: "b1"), op("p-tom", "Tom", epoch: 4, block: "b1", type: "insert")]
        #expect(RunTrust.decide(block: "b1", history: h, me: me, practiceEditedByMe: false, approval: nil, docEpoch: 6) == .ask(lastEditedBy: "claude:tagger"))
        // a move by someone else doesn't change the code
        let moved = [op("p-aoife", "Aoife", epoch: 7, block: "b1", type: "move")] + h.dropFirst()
        #expect(RunTrust.decide(block: "b1", history: Array(moved), me: me, practiceEditedByMe: false, approval: nil, docEpoch: 7) == .run)
    }

    @Test func practiceEditsAreYours() {
        let h = [op("p-claude", "claude:x", epoch: 6, block: "b1")]
        #expect(RunTrust.decide(block: "b1", history: h, me: me, practiceEditedByMe: true, approval: nil, docEpoch: 6) == .run)
    }

    @Test func unknownAsks() {
        #expect(RunTrust.decide(block: "b1", history: [], me: me, practiceEditedByMe: false, approval: nil, docEpoch: 1) == .ask(lastEditedBy: "someone else"))
        #expect(RunTrust.decide(block: "b1", history: nil, me: me, practiceEditedByMe: false, approval: nil, docEpoch: 1) == .ask(lastEditedBy: "someone (couldn't check: offline)"))
    }

    @Test func approvalHoldsUntilSomeoneElseChangesTheDoc() {
        let h = [op("p-claude", "claude:x", epoch: 6, block: "b1")]
        let a = RunTrust.approval(history: h, me: me, docEpoch: 6)
        #expect(a == RunTrust.Approval(docEpoch: 6, othersEpoch: 6))
        #expect(RunTrust.decide(block: "b1", history: h, me: me, practiceEditedByMe: false, approval: a, docEpoch: 6) == .run)
        // your own later edit elsewhere in the doc: still approved
        let mine = [op("p-tom", "Tom", epoch: 7, block: "b2")] + h
        #expect(RunTrust.decide(block: "b1", history: mine, me: me, practiceEditedByMe: false, approval: a, docEpoch: 7) == .run)
        // someone else changes anything in the doc: ask again
        let theirs = [op("p-aoife", "Aoife", epoch: 8, block: "b3")] + mine
        #expect(RunTrust.decide(block: "b1", history: theirs, me: me, practiceEditedByMe: false, approval: a, docEpoch: 8) == .ask(lastEditedBy: "claude:x"))
        // offline: holds only while the doc hasn't moved at all
        #expect(RunTrust.decide(block: "b1", history: nil, me: me, practiceEditedByMe: false, approval: a, docEpoch: 6) == .run)
        #expect(RunTrust.decide(block: "b1", history: nil, me: me, practiceEditedByMe: false, approval: a, docEpoch: 7) == .ask(lastEditedBy: "someone (couldn't check: offline)"))
    }

    @Test func approvalWithNoOtherAuthorYet() {
        // asked because the block was unknown; nobody else in the history
        let h = [op("p-tom", "Tom", epoch: 3, block: "b2")]
        let a = RunTrust.approval(history: h, me: me, docEpoch: 3)
        #expect(a.othersEpoch == nil)
        #expect(RunTrust.decide(block: "b1", history: h, me: me, practiceEditedByMe: false, approval: a, docEpoch: 3) == .run)
        let later = [op("p-claude", "claude:x", epoch: 4, block: "b9")] + h
        #expect(RunTrust.decide(block: "b1", history: later, me: me, practiceEditedByMe: false, approval: a, docEpoch: 4) == .ask(lastEditedBy: "someone else"))
    }

    @Test func nameFallbackForOlderServers() {
        let old = RunTrust.Me(principalID: nil, name: "Tom")
        let h = [DocHistoryEntry(opID: "o", principalName: "Tom", principalKind: "human", epoch: 2, targetBlock: "b1", opType: "replace")]
        #expect(RunTrust.decide(block: "b1", history: h, me: old, practiceEditedByMe: false, approval: nil, docEpoch: 2) == .run)
        let nobody = RunTrust.Me(principalID: nil, name: nil)
        #expect(RunTrust.decide(block: "b1", history: h, me: nobody, practiceEditedByMe: false, approval: nil, docEpoch: 2) == .ask(lastEditedBy: "Tom"))
    }

    @Test func approvalsPersistPerServer() throws {
        let suite = "taisce.tests.approvals.\(UUID().uuidString)"
        let d = try #require(UserDefaults(suiteName: suite))
        defer { d.removePersistentDomain(forName: suite) }
        let a = RunApprovals(defaults: d, server: "https://taisce.test")
        a.approve("doc1", .init(docEpoch: 3, othersEpoch: 2))
        #expect(RunApprovals(defaults: d, server: "https://taisce.test").approval(for: "doc1") == .init(docEpoch: 3, othersEpoch: 2))
        #expect(RunApprovals(defaults: d, server: "https://other.test").approval(for: "doc1") == nil)
        a.forget()
        #expect(a.approval(for: "doc1") == nil)
    }

    @Test func historyDecodesProvenance() throws {
        let json = """
        [{"op":{"id":"0199a0b0-c0d0-7abc-8def-0123456789ab","doc_id":"d1","principal":"0199A0B0-0000-7000-8000-000000000001","epoch_applied":4,
                "kind":{"op":"replace","target":"0199A0B0-0000-7000-8000-0000000000BB","content":"x"}},"principal_name":"claude","principal_kind":"agent"},
         {"op":{"id":"o2","doc_id":"d1","principal":"p2","epoch_applied":3,"kind":{"op":"insert","block_id":"b2","parent_id":null,"order_key":"a","block_type":"code","content":"y"}},"principal_name":"tom","principal_kind":"human"},
         {"op":{"id":"o3","doc_id":"d1","epoch_applied":2,"kind":{"op":"rename_doc","title":"T"}},"principal_name":"tom","principal_kind":"human"},
         {"op":{"id":"o4","doc_id":"d1","epoch_applied":null},"principal_name":"tom","principal_kind":"human"}]
        """
        let rows = try JSONDecoder().decode([DocHistoryEntry].self, from: Data(json.utf8))
        #expect(rows[0].principalID == "0199a0b0-0000-7000-8000-000000000001")
        #expect(rows[0].targetBlock == "0199a0b0-0000-7000-8000-0000000000bb")
        #expect(rows[0].opType == "replace" && rows[0].epoch == 4)
        #expect(rows[1].targetBlock == "b2" && rows[1].opType == "insert")
        #expect(rows[2].targetBlock == nil && rows[2].opType == "rename_doc")
        #expect(rows[3].applied == false && rows[3].principalID == nil && rows[3].opType == nil)
    }
}

@Suite struct SandboxMigrationTests {
    func tempDir(_ name: String) throws -> URL {
        let d = FileManager.default.temporaryDirectory.appendingPathComponent("taisce-mig-\(name)-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: d, withIntermediateDirectories: true)
        return d
    }

    /// A fake container: a cache with a doc and two unsent writes (still in
    /// the WAL), and a preferences plist.
    func fakeContainer(prefs: [String: Any]) async throws -> URL {
        let data = try tempDir("container").appendingPathComponent("Data", isDirectory: true)
        let support = data.appendingPathComponent("Library/Application Support", isDirectory: true)
        try FileManager.default.createDirectory(at: support, withIntermediateDirectories: true)
        try await {
            let cache = try Cache(path: support.appendingPathComponent("cache-taisce.null.ie-0.sqlite").path)
            try await cache.storeDoc(DocTree(doc: DocSummary(id: "d1", parentID: nil, title: "Notes", currentEpoch: 3),
                                             roots: [BlockNode(block: Block(id: "b1", docID: "d1", parentID: nil, orderKey: "a", blockType: .paragraph, content: "hi"))]))
            _ = try await cache.enqueueTodoAdd(date: "2026-10-02", text: "buy milk")
            _ = try await cache.enqueueTodoAdd(date: "2026-10-02", text: "call Ann")
        }()
        try Data("other".utf8).write(to: support.appendingPathComponent("unrelated.txt"))
        let prefsDir = data.appendingPathComponent("Library/Preferences", isDirectory: true)
        try FileManager.default.createDirectory(at: prefsDir, withIntermediateDirectories: true)
        let plist = try PropertyListSerialization.data(fromPropertyList: prefs, format: .binary, options: 0)
        try plist.write(to: prefsDir.appendingPathComponent("ie.null.taisce.plist"))
        return data
    }

    @Test func migratesCacheWithOutboxAndPreferences() async throws {
        let container = try await fakeContainer(prefs: [
            "serverURL": "https://taisce.null.ie", "pins-taisce.null.ie-0": ["d1"], "doc.textSize": 2, "workspace-taisce.null.ie-0": "w1",
        ])
        let dest = try tempDir("support").appendingPathComponent("ie.null.taisce", isDirectory: true)
        let suite = "taisce.tests.migration.\(UUID().uuidString)"
        let defaults = try #require(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        // a key the new location already has keeps its value
        defaults.set(3, forKey: "doc.textSize")

        let r = SandboxMigration.run(containerData: container, supportDestination: dest, bundleID: "ie.null.taisce", defaults: defaults, domain: suite)
        #expect(r.complete)
        #expect(r.copied.contains("Application Support/cache-taisce.null.ie-0.sqlite"))
        #expect(!r.copied.contains { $0.contains("unrelated") })
        #expect(r.importedKeys == ["pins-taisce.null.ie-0", "serverURL", "workspace-taisce.null.ie-0"])
        #expect(r.keptKeys == ["doc.textSize"])
        #expect(defaults.string(forKey: "serverURL") == "https://taisce.null.ie")
        #expect(defaults.stringArray(forKey: "pins-taisce.null.ie-0") == ["d1"])
        #expect(defaults.integer(forKey: "doc.textSize") == 3)
        #expect(defaults.string(forKey: SandboxMigration.doneKey) != nil)

        // the app's Cache, opened on the result, has the doc and the unsent writes
        let cache = try Cache(path: dest.appendingPathComponent("cache-taisce.null.ie-0.sqlite").path)
        #expect(try await cache.pendingOutbox().count == 2)
        #expect(try await cache.doc("d1")?.title == "Notes")
        #expect(try await cache.blocks(of: "d1").map(\.content) == ["hi"])

        // the container is untouched
        #expect(FileManager.default.fileExists(atPath: container.appendingPathComponent("Library/Application Support/cache-taisce.null.ie-0.sqlite").path))
        #expect(FileManager.default.fileExists(atPath: container.appendingPathComponent("Library/Preferences/ie.null.taisce.plist").path))

        // idempotent: a second launch does nothing
        let again = SandboxMigration.run(containerData: container, supportDestination: dest, bundleID: "ie.null.taisce", defaults: defaults, domain: suite)
        #expect(again.alreadyDone && again.copied.isEmpty)
        #expect(SandboxMigration.logLines(again).isEmpty)
        #expect(SandboxMigration.logLines(r).first?.contains("sandbox container migration") == true)
    }

    @Test func neverOverwritesAnExistingCacheAndRetriesWithoutTheMarker() async throws {
        let container = try await fakeContainer(prefs: ["serverURL": "https://taisce.null.ie"])
        let dest = try tempDir("support2")
        let suite = "taisce.tests.migration.\(UUID().uuidString)"
        let defaults = try #require(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        let existing = dest.appendingPathComponent("cache-taisce.null.ie-0.sqlite")
        try Data("mine".utf8).write(to: existing)
        let r = SandboxMigration.run(containerData: container, supportDestination: dest, bundleID: "ie.null.taisce", defaults: defaults, domain: suite)
        #expect(r.kept == ["Application Support/cache-taisce.null.ie-0.sqlite"])
        #expect(r.copied.isEmpty)
        #expect(try Data(contentsOf: existing) == Data("mine".utf8))
        // the marker means a later run is a no-op even if the file goes away
        try FileManager.default.removeItem(at: existing)
        #expect(SandboxMigration.run(containerData: container, supportDestination: dest, bundleID: "ie.null.taisce", defaults: defaults, domain: suite).alreadyDone)
    }

    @Test func noContainerIsDoneAndAnUnreadableOneIsRetried() throws {
        let suite = "taisce.tests.migration.\(UUID().uuidString)"
        let defaults = try #require(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        let missing = FileManager.default.temporaryDirectory.appendingPathComponent("nope-\(UUID().uuidString)/Data")
        let dest = try tempDir("support3")
        let r = SandboxMigration.run(containerData: missing, supportDestination: dest, bundleID: "ie.null.taisce", defaults: defaults, domain: suite)
        #expect(r.noContainer && r.complete)

        let suite2 = "taisce.tests.migration.\(UUID().uuidString)"
        let d2 = try #require(UserDefaults(suiteName: suite2))
        defer { d2.removePersistentDomain(forName: suite2) }
        let locked = try tempDir("locked").appendingPathComponent("Data", isDirectory: true)
        try FileManager.default.createDirectory(at: locked.appendingPathComponent("Library"), withIntermediateDirectories: true)
        try FileManager.default.setAttributes([.posixPermissions: 0o000], ofItemAtPath: locked.path)
        defer { try? FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: locked.path) }
        let u = SandboxMigration.run(containerData: locked, supportDestination: dest, bundleID: "ie.null.taisce", defaults: d2, domain: suite2)
        #expect(u.sourceUnreadable && !u.complete)
        #expect(d2.string(forKey: SandboxMigration.doneKey) == nil, "retried next launch")
        #expect(SandboxMigration.logLines(u).first?.contains("incomplete") == true)
    }
}
