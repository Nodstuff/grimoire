import Foundation
import Synchronization
@testable import GrimoireKit

/// A scripted server behind `URLProtocol`. Each `MockServer` owns its own
/// URLSession, routed by a header, so Swift Testing's parallel tests don't
/// share handlers.
final class MockServer: Sendable {
    struct Reply: Sendable {
        var status = 200
        var chunks: [Data]
        var contentType = "application/json"
        /// fail the connection after the chunks, instead of finishing cleanly
        var failAfter = false

        static func json(_ s: String) -> Reply { Reply(chunks: [Data(s.utf8)]) }
        static func sse(_ chunks: String...) -> Reply { Reply(chunks: chunks.map { Data($0.utf8) }, contentType: "text/event-stream") }
    }

    typealias Handler = @Sendable (URLRequest) -> Reply

    let id = UUID().uuidString
    private let handler: Handler
    private let log = Mutex<[URLRequest]>([])

    init(_ handler: @escaping Handler) {
        self.handler = handler
        MockURLProtocol.servers.withLock { $0[id] = self }
    }

    deinit {
        MockURLProtocol.servers.withLock { $0[id] = nil }
    }

    var requests: [URLRequest] { log.withLock { $0 } }

    func handle(_ r: URLRequest) -> Reply {
        log.withLock { $0.append(r) }
        return handler(r)
    }

    var session: URLSession {
        let c = URLSessionConfiguration.ephemeral
        c.protocolClasses = [MockURLProtocol.self]
        c.httpAdditionalHeaders = [MockURLProtocol.header: id]
        return URLSession(configuration: c)
    }

    func client() -> APIClient {
        // a URL literal in a test helper: not library code
        APIClient(config: ServerConfig(baseURL: URL(string: "http://mock.local")!), session: session)
    }
}

final class MockURLProtocol: URLProtocol, @unchecked Sendable {
    static let header = "X-Mock-Server"
    static let servers = Mutex<[String: MockServer]>([:])

    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }

    override func startLoading() {
        guard let key = request.value(forHTTPHeaderField: Self.header),
              let server = Self.servers.withLock({ $0[key] }),
              let url = request.url
        else {
            client?.urlProtocol(self, didFailWithError: URLError(.cannotConnectToHost))
            return
        }
        // URLProtocol sees the body as a stream for some requests
        var req = request
        if req.httpBody == nil, let stream = req.httpBodyStream {
            req.httpBody = Data(reading: stream)
        }
        let reply = server.handle(req)
        let response = HTTPURLResponse(url: url, statusCode: reply.status, httpVersion: "HTTP/1.1", headerFields: ["Content-Type": reply.contentType])
        if let response { client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed) }
        for chunk in reply.chunks { client?.urlProtocol(self, didLoad: chunk) }
        if reply.failAfter {
            client?.urlProtocol(self, didFailWithError: URLError(.networkConnectionLost))
        } else {
            client?.urlProtocolDidFinishLoading(self)
        }
    }

    override func stopLoading() {}
}

extension Data {
    init(reading stream: InputStream) {
        self.init()
        stream.open()
        defer { stream.close() }
        var buf = [UInt8](repeating: 0, count: 4096)
        while stream.hasBytesAvailable {
            let n = stream.read(&buf, maxLength: buf.count)
            if n <= 0 { break }
            append(buf, count: n)
        }
    }
}

extension URLRequest {
    var query: [String: String] {
        guard let url, let items = URLComponents(url: url, resolvingAgainstBaseURL: false)?.queryItems else { return [:] }
        return Dictionary(items.map { ($0.name, $0.value ?? "") }, uniquingKeysWith: { _, b in b })
    }

    var path: String { url?.path() ?? "" }
}

/// JSON fixtures in the daemon's real shapes (captured from /api/docs and /api/doc/{id}).
enum Fixture {
    static func summary(_ id: String, title: String, parent: String? = nil, epoch: Int = 1) -> String {
        let p = parent.map { "\"\($0)\"" } ?? "null"
        return """
        {"created_by":"u","current_epoch":\(epoch),"id":"\(id)","is_canvas":false,"is_shared":false,\
        "is_tended":false,"owner_tended":false,"parent_id":\(p),"review_policy":null,"sort_key":"i",\
        "status":null,"title":"\(title)"}
        """
    }

    static func block(_ id: String, doc: String, type: String = "paragraph", content: String, parent: String? = nil, epoch: Int = 1) -> String {
        let p = parent.map { "\"\($0)\"" } ?? "null"
        let c = String(data: (try? JSONEncoder().encode(content)) ?? Data(), encoding: .utf8) ?? "\"\""
        return """
        {"block_type":"\(type)","content":\(c),"created_by":"u","deleted":false,"doc_id":"\(doc)",\
        "epoch":\(epoch),"id":"\(id)","order_key":"i","parent_id":\(p),"refers_to":null}
        """
    }

    static func docTree(_ id: String, title: String, epoch: Int = 1, roots: String) -> String {
        """
        {"doc":{"created_by":"u","current_epoch":\(epoch),"id":"\(id)","parent_id":null,\
        "review_policy":null,"sort_key":"i","status":null,"title":"\(title)"},"roots":[\(roots)]}
        """
    }

    static func change(_ seq: Int, doc: String, kind: String, epoch: Int? = nil) -> String {
        let e = epoch.map(String.init) ?? "null"
        return #"{"seq":\#(seq),"doc_id":"\#(doc)","kind":"\#(kind)","epoch":\#(e),"at":"2026-10-01T09:00:00Z"}"#
    }
}
