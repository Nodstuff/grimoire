import Foundation
import Testing
@testable import TaisceKit

@Suite struct MetaAPITests {
    @Test func historyDecodesPrincipalsAndOpTime() async throws {
        // a v7 id carries its write time in the first 48 bits
        let server = MockServer { r in
            #expect(r.path == "/api/doc/d1/history")
            return .json("""
            [{"op":{"id":"0199a0b0-c0d0-7abc-8def-0123456789ab","doc_id":"d1","epoch_applied":4},"principal_name":"claude","principal_kind":"agent"},
             {"op":{"id":"not-a-uuid","doc_id":"d1","epoch_applied":null},"principal_name":"tom","principal_kind":"human"}]
            """)
        }
        let rows = try await server.client().docHistory("d1")
        #expect(rows.map(\.principalName) == ["claude", "tom"])
        #expect(rows.map(\.applied) == [true, false])
        #expect(rows[0].date == Date(timeIntervalSince1970: Double(0x0199a0b0c0d0) / 1000))
        #expect(rows[1].date == nil)
    }

    @Test func uuidV7DateRejectsOtherVersions() {
        #expect(DocHistoryEntry.uuidV7Date("0199a0b0-c0d0-4abc-8def-0123456789ab") == nil)
        #expect(DocHistoryEntry.uuidV7Date("0199a0b0-c0d0-7abc-8def-0123456789ab") != nil)
    }

    @Test func parseHintSendsTextAndReadsDue() async throws {
        let server = MockServer { _ in
            .json(#"{"text":"call Ann","deadline":"2026-10-03","due_time":"15:00","alert_at":"2026-10-03T15:00"}"#)
        }
        let hint = try await server.client().todoParse("call Ann due fri 3pm")
        #expect(server.requests[0].query == ["text": "call Ann due fri 3pm"])
        #expect(hint.text == "call Ann")
        #expect(hint.due == Due(year: 2026, month: 10, day: 3, hour: 15, minute: 0))
    }

    @Test func parseHintWithoutDeadline() async throws {
        let server = MockServer { _ in .json(#"{"text":"buy milk","deadline":null,"due_time":null,"alert_at":null}"#) }
        let hint = try await server.client().todoParse("buy milk")
        #expect(hint.due == nil && hint.warning == nil)
    }
}
