import Foundation
import Testing
@testable import GrimoireKit

@Suite struct SSEParserTests {
    func parse(_ chunks: [String]) -> [SSEOutput] {
        var p = SSEParser()
        return chunks.flatMap { p.feed(Data($0.utf8)) }
    }

    @Test func idEventData() {
        let out = parse(["id: 7\nevent: change\ndata: {\"seq\":7}\n\n"])
        #expect(out == [.event(SSEEvent(id: "7", event: "change", data: "{\"seq\":7}"))])
    }

    @Test func commentsAreHeartbeats() {
        let out = parse([": ping\n\n", ":\n"])
        #expect(out == [.comment("ping"), .comment("")])
    }

    @Test func multiLineDataJoinsWithNewline() {
        let out = parse(["data: one\ndata:two\ndata:  three\n\n"])
        #expect(out == [.event(SSEEvent(id: nil, event: "message", data: "one\ntwo\n three"))])
    }

    @Test func retryOnlyAcceptsDigits() {
        let out = parse(["retry: 3000\nretry: soon\nretry: 12a\n\n"])
        #expect(out == [.retry(3000)])
    }

    @Test func chunksSplitAnywhereAndCRLF() {
        let s = "id: 1\r\nevent: change\r\ndata: a\r\n\r\nid: 2\rdata: b\r\r"
        // byte-at-a-time is the worst case for CRLF split across reads
        let out = parse(s.map { String($0) })
        #expect(out == [
            .event(SSEEvent(id: "1", event: "change", data: "a")),
            .event(SSEEvent(id: "2", event: "message", data: "b")),
        ])
    }

    @Test func idPersistsAndEmptyEventsDontDispatch() {
        var p = SSEParser()
        var out = p.feed(Data("id: 9\n\n".utf8))
        #expect(out.isEmpty)
        #expect(p.lastEventID == "9")
        out = p.feed(Data("data: x\n\n".utf8))
        #expect(out == [.event(SSEEvent(id: "9", event: "message", data: "x"))])
    }

    @Test func eventTypeResetsBetweenEvents() {
        let out = parse(["event: change\ndata: a\n\ndata: b\n\n"])
        #expect(out.count == 2)
        #expect(out.last == .event(SSEEvent(id: nil, event: "message", data: "b")))
    }

    @Test func leadingBOMAndFieldWithoutColon() {
        let out = parse(["\u{FEFF}data\n\n"])
        #expect(out == [.event(SSEEvent(id: nil, event: "message", data: ""))])
    }

    @Test func incompleteEventIsHeldUntilBlankLine() {
        var p = SSEParser()
        #expect(p.feed(Data("data: half".utf8)).isEmpty)
        #expect(p.feed(Data("\n".utf8)).isEmpty)
        #expect(p.feed(Data("\n".utf8)) == [.event(SSEEvent(id: nil, event: "message", data: "half"))])
    }
}
