import Foundation
import SwiftUI
import Synchronization
import Testing
import TaisceKit
@testable import Taisce

/// Records requests and answers each with a canned JSON body.
final class RecordingProtocol: URLProtocol {
    static let log = Mutex<[URLRequest]>([])
    static let reply = Mutex<String>("{}")

    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }

    override func startLoading() {
        var req = request
        if req.httpBody == nil, let stream = req.httpBodyStream {
            stream.open()
            var data = Data()
            var buf = [UInt8](repeating: 0, count: 4096)
            while stream.hasBytesAvailable {
                let n = stream.read(&buf, maxLength: buf.count)
                if n <= 0 { break }
                data.append(buf, count: n)
            }
            stream.close()
            req.httpBody = data
        }
        Self.log.withLock { $0.append(req) }
        let body = Self.reply.withLock { $0 }
        let response = HTTPURLResponse(url: request.url!, statusCode: 200, httpVersion: "HTTP/1.1", headerFields: ["Content-Type": "application/json"])!
        client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: Data(body.utf8))
        client?.urlProtocolDidFinishLoading(self)
    }

    override func stopLoading() {}

    static func client() -> APIClient {
        let config = URLSessionConfiguration.ephemeral
        config.protocolClasses = [RecordingProtocol.self]
        return APIClient(config: ServerConfig(baseURL: URL(string: "http://mock.invalid")!), session: URLSession(configuration: config))
    }
}

private let a = Workspace(id: "wa", name: "Alpha", sortKey: "c", docCount: 558)
private let b = Workspace(id: "wb", name: "Beta", sortKey: "i", docCount: 1)
private let c = Workspace(id: "wc", name: "Gamma", sortKey: "r", docCount: 0)

@Suite(.serialized) struct ManageWorkspacesTests {
    func body(_ r: URLRequest?) throws -> [String: Any] {
        let data = try #require(r?.httpBody)
        return try #require(JSONSerialization.jsonObject(with: data) as? [String: Any])
    }

    @Test func renameRecolourAndReorderArePatches() async throws {
        RecordingProtocol.log.withLock { $0 = [] }
        RecordingProtocol.reply.withLock { $0 = #"{"id":"wa","name":"Work","doc_ids":[],"doc_count":3}"# }
        let api = RecordingProtocol.client()
        _ = try await api.updateWorkspace("wa", name: "Work")
        _ = try await api.updateWorkspace("wa", color: "#95c99b")
        _ = try await api.updateWorkspace("wa", sortKey: "f")
        let reqs = RecordingProtocol.log.withLock { $0 }
        #expect(reqs.map(\.httpMethod) == ["PATCH", "PATCH", "PATCH"])
        #expect(reqs.allSatisfy { $0.url?.path() == "/api/workspaces/wa" })
        let rename = try body(reqs[0]), colour = try body(reqs[1]), sort = try body(reqs[2])
        #expect(rename as NSDictionary == ["name": "Work"], "absent fields are kept")
        #expect(colour as NSDictionary == ["color": "#95c99b"])
        #expect(sort as NSDictionary == ["sort_key": "f"])
    }

    @Test func deleteIsADelete() async throws {
        RecordingProtocol.log.withLock { $0 = [] }
        RecordingProtocol.reply.withLock { $0 = #"{"deleted":"wa","unlabelled":2}"# }
        try await RecordingProtocol.client().deleteWorkspace("wa")
        let r = try #require(RecordingProtocol.log.withLock { $0.last })
        #expect(r.httpMethod == "DELETE" && r.url?.path() == "/api/workspaces/wa")
    }

    @Test func reorderKeysLandBetweenTheNewNeighbours() {
        // Gamma to the top: before Alpha
        let top = WorkspaceManagement.sortKey(moving: "wc", to: 0, in: [a, b, c])
        #expect(OrderKey.isValid(top) && top < "c")
        // Alpha between Beta and Gamma
        let mid = WorkspaceManagement.sortKey(moving: "wa", to: 1, in: [a, b, c])
        #expect(OrderKey.isValid(mid) && mid > "i" && mid < "r")
        // Alpha to the end
        let end = WorkspaceManagement.sortKey(moving: "wa", to: 2, in: [a, b, c])
        #expect(OrderKey.isValid(end) && end > "r")
        // List.onMove: dragging row 0 to the slot after row 2 means index 2 of the rest
        #expect(WorkspaceManagement.destination(from: 0, to: 3) == 2)
        #expect(WorkspaceManagement.destination(from: 2, to: 0) == 0)
    }

    @Test func deletingTheCurrentWorkspaceFallsBackToTheFirstRemaining() {
        #expect(WorkspaceManagement.fallback(afterDeleting: "wa", from: [a, b, c]) == .id("wb"))
        #expect(WorkspaceManagement.fallback(afterDeleting: "wb", from: [a, b, c]) == .id("wa"))
        #expect(WorkspaceManagement.fallback(afterDeleting: "wa", from: [a]) == .unsorted)
        // and the picker agrees once the stored one is gone
        #expect(WorkspacePicker(workspaces: [b, c], unsortedCount: 558, stored: .id("wa")).current == .id("wb"))
    }

    @Test func deleteConfirmationSaysWhereTheDocsGo() {
        let work = Workspace(id: "w", name: "Work", docCount: 558)
        #expect(WorkspaceManagement.deletePrompt(work) == "Delete Work? Its 558 docs move to Unsorted. No docs are deleted.")
        #expect(WorkspaceManagement.deletePrompt(b) == "Delete Beta? Its 1 doc moves to Unsorted. No docs are deleted.")
    }

    @Test func swatchesAreSixThemeColours() {
        #expect(WorkspacePalette.colors.count == 6)
        #expect(WorkspacePalette.colors.allSatisfy { Color(workspaceHex: $0) != nil })
    }
}

