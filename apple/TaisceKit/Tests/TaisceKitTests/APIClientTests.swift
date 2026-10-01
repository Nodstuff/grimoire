import Foundation
import Testing
@testable import TaisceKit

@Suite struct APIClientTests {
    @Test func decodesTreeWithDecorations() async throws {
        let server = MockServer { _ in .json("[\(Fixture.summary("d1", title: "A")),\(Fixture.summary("d2", title: "B", parent: "d1"))]") }
        let docs = try await server.client().tree()
        #expect(docs.map(\.id) == ["d1", "d2"])
        #expect(docs[1].parentID == "d1" && docs[1].sortKey == "i")
    }

    @Test func errorEnvelopeOn200Throws() async throws {
        let server = MockServer { _ in .json(#"{"error":"doc not found"}"#) }
        await #expect(throws: APIError.server("doc not found")) {
            _ = try await server.client().doc("nope")
        }
    }

    @Test func httpErrorStatusThrows() async throws {
        let server = MockServer { _ in MockServer.Reply(status: 502, chunks: [Data("bad gateway".utf8)]) }
        await #expect(throws: APIError.http(status: 502)) {
            _ = try await server.client().tree()
        }
    }

    @Test func jsonErrorWithStatusCarriesTheMessage() async throws {
        let server = MockServer { _ in MockServer.Reply(status: 400, chunks: [Data(#"{"error":"since must be an integer"}"#.utf8)]) }
        await #expect(throws: APIError.server("since must be an integer")) {
            _ = try await server.client().changes(since: 0)
        }
    }

    @Test func htmlFallbackIsNotAnAPIResponse() async throws {
        let server = MockServer { _ in MockServer.Reply(chunks: [Data("<!doctype html>".utf8)], contentType: "text/html; charset=utf-8") }
        await #expect(throws: APIError.notAPIRoute("/api/changes")) {
            _ = try await server.client().changes(since: 0)
        }
    }

    @Test func searchAndChangesBuildQueries() async throws {
        let server = MockServer { r in
            r.path == "/api/search" ? .json("[]") : .json(#"{"seq":0,"changes":[],"more":false}"#)
        }
        let api = server.client()
        _ = try await api.search("a b&c", scope: "d1")
        _ = try await api.changes(since: 12, limit: 50)
        #expect(server.requests[0].query == ["q": "a b&c", "scope": "d1"])
        #expect(server.requests[1].query == ["since": "12", "limit": "50"])
    }

    @Test func tokenProviderAddsBearer() async throws {
        struct Fixed: TokenProvider { func token() async throws -> String? { "t0k" } }
        let server = MockServer { _ in .json("[]") }
        let base = try #require(URL(string: "http://mock.local/sub/"))
        let api = APIClient(config: ServerConfig(baseURL: base, tokenProvider: Fixed()), session: server.session)
        _ = try await api.tree()
        #expect(server.requests[0].value(forHTTPHeaderField: "Authorization") == "Bearer t0k")
        #expect(server.requests[0].url?.absoluteString == "http://mock.local/sub/api/docs")
    }

    @Test func proposeEncodesOpKinds() throws {
        let req = ProposeRequest(docID: "d", baseEpoch: 1, ops: [
            .insert(blockID: nil, parentID: nil, orderKey: "m", type: .paragraph, content: "x"),
            .move(target: "b", newParent: "h", newOrderKey: "z"),
            .delete(target: "c"),
        ])
        let obj = try #require(try JSONSerialization.jsonObject(with: JSONEncoder().encode(req)) as? [String: Any])
        let kinds = try #require((obj["ops"] as? [[String: Any]])?.compactMap { $0["kind"] as? [String: Any] })
        #expect(kinds[0]["op"] as? String == "insert" && kinds[0]["block_type"] as? String == "paragraph")
        #expect(kinds[0]["block_id"] == nil, "absent → the server mints the id")
        #expect(kinds[0].keys.contains("parent_id"))
        #expect(kinds[1]["new_parent"] as? String == "h" && kinds[1]["new_order_key"] as? String == "z")
        #expect(kinds[2]["op"] as? String == "delete")
        #expect(obj["base_epoch"] as? Int == 1)
    }

    @Test func todoDueDecodesAndSendsUntil() async throws {
        let server = MockServer { _ in
            .json(#"{"doc_id":"t","epoch":12,"now":"2026-10-01T12:00","until":"2026-10-01","default_alert_time":"09:00","items":[{"date":"2026-09-30","id":"0-ab","text":"rent","deadline":"2026-10-01","due_time":"10:00","alert_at":"2026-10-01T10:00","overdue":true},{"date":"2026-10-01","id":"1-cd","text":"call","deadline":"2026-10-01","alert_at":"2026-10-01T09:00","overdue":true,"carried_from":"2026-09-29"}]}"#)
        }
        let due = try await server.client().todoDue(until: "2026-10-01")
        #expect(server.requests[0].path == "/api/todo/due" && server.requests[0].query == ["until": "2026-10-01"])
        #expect(due.items.map(\.id) == ["2026-09-30/0-ab", "2026-10-01/1-cd"])
        #expect(due.items[0].due == Due(year: 2026, month: 10, day: 1, hour: 10, minute: 0))
        #expect(due.items[1].due?.hasTime == false && due.items[1].carriedFrom == "2026-09-29")
    }

    @Test func todoDueWithoutUntil() async throws {
        let server = MockServer { _ in
            .json(#"{"doc_id":null,"epoch":0,"now":"2026-10-01T12:00","until":null,"default_alert_time":"09:00","items":[]}"#)
        }
        let due = try await server.client().todoDue()
        #expect(server.requests[0].query.isEmpty && due.until == nil && due.docID == nil)
    }

    @Test func badUntilIs400JSON() async throws {
        let server = MockServer { _ in MockServer.Reply(status: 400, chunks: [Data(#"{"error":"until: bad date"}"#.utf8)]) }
        await #expect(throws: APIError.server("until: bad date")) {
            _ = try await server.client().todoDue(until: "fri")
        }
    }

    @Test func deadlineBodySplitsDateAndTime() throws {
        func body(_ d: Due?) throws -> [String: Any] {
            let data = try JSONEncoder().encode(APIClient.DeadlineBody(date: "2026-10-01", itemID: "0-ab", deadline: d))
            return try #require(try JSONSerialization.jsonObject(with: data) as? [String: Any])
        }
        let timed = try body(Due("2026-10-02 14:30"))
        #expect(timed["deadline"] as? String == "2026-10-02" && timed["due_time"] as? String == "14:30")
        let dateOnly = try body(Due("2026-10-02"))
        #expect(dateOnly["deadline"] as? String == "2026-10-02" && dateOnly["due_time"] == nil)
        let cleared = try body(nil)
        #expect(cleared["deadline"] is NSNull && cleared.keys.contains("deadline"))
    }

    @Test func todoDayDecodes() async throws {
        let server = MockServer { _ in
            .json(#"{"carried":0,"date":"2026-10-01","doc_id":"t","epoch":11,"items":[{"id":"0-1","text":"x","done":true,"overdue":false,"due_soon":false}],"prev_date":null,"today":"2026-10-01"}"#)
        }
        let day = try await server.client().todoAdd(date: "2026-10-01", text: "x")
        #expect(day.items.first?.done == true && day.prevDate == nil && day.epoch == 11)
    }
}
