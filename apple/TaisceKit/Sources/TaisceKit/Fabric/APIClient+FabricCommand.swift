import Foundation

/// The viewer API of Slices 2–5 (ADR 0008 §11), as the app uses it; a fake
/// in tests. Every route answers only the person's own app.
public protocol FabricService: FleetService {
    func turns(_ session: FleetSessionKey, before: Int?, limit: Int) async throws -> FabricTurnPage
    /// The session's turns ended after `after`, OLDEST first (`after=`). A
    /// server from before `after=` ignores it and answers the newest page,
    /// newest first: `FabricTurnPage.isAfterPage` tells them apart.
    func turns(_ session: FleetSessionKey, after: Int, limit: Int) async throws -> FabricTurnPage
    func turns(since: Int, limit: Int) async throws -> [FabricTurn]
    /// Strictly older than the turn `beforeID`, which ended at `before`,
    /// newest first (`before=` + `before_id=`: no turn ending in the same
    /// second is skipped or repeated). A server without `before_id` gets
    /// today's `before=` a second later (the caller keeps each turn once).
    func turns(_ session: FleetSessionKey, before: Int, beforeID: String, limit: Int) async throws -> FabricTurnPage
    /// The turns stored after `afterSeq` in the order the server stored
    /// them (`after_seq=`, late replays included), with `more`. Nil from a
    /// server that does not page by `seq` (it refuses the parameter, or
    /// ignores it and answers turns without `seq`): ask by time instead.
    func turns(_ session: FleetSessionKey, afterSeq: Int, limit: Int) async throws -> FabricTurnPage?
    func attention() async throws -> [FabricAttentionItem]
    func orders(session: FleetSessionKey?, limit: Int) async throws -> [FabricOrderView]
    func order(_ id: String) async throws -> FabricOrderView
    func postOrder(_ body: FabricSignedBody) async throws -> FabricOrderView
    func withdrawOrder(_ id: String) async throws -> FabricOrderView
    func settings() async throws -> FabricSettingsView
    func putSettings(_ body: FabricSignedBody) async throws -> FabricSettingsView
    func projects() async throws -> [FabricProject]
    func setProject(_ name: String, textOff: Bool?, privateNotifications: Bool?) async throws -> FabricProject
    func pair(runtimeID: String, pairingID: String, sealed: Data) async throws -> FabricPairingView
    func pairing(_ id: String) async throws -> FabricPairingView
    func follow(_ session: FleetSessionKey, _ on: Bool) async throws -> FleetSession
    func dismissAttention(_ id: String) async throws
    func conversations() async throws -> [FabricConversation]
    func conversation(_ id: String, limit: Int) async throws -> FabricConversationPage
    func continueConversation(_ id: String) async throws
    func stopConversation(_ id: String) async throws
}

extension FabricService {
    /// A service without tie-safe paging: the old `before=`, a second on.
    public func turns(_ session: FleetSessionKey, before: Int, beforeID: String, limit: Int) async throws -> FabricTurnPage {
        try await turns(session, before: before + 1, limit: limit)
    }

    /// A service without `seq`: never pages by it.
    public func turns(_ session: FleetSessionKey, afterSeq: Int, limit: Int) async throws -> FabricTurnPage? { nil }
}

extension APIClient: FabricService {
    public func turns(_ session: FleetSessionKey, before: Int?, limit: Int = 30) async throws -> FabricTurnPage {
        var q = [URLQueryItem(name: "limit", value: String(limit))]
        if let before { q.append(URLQueryItem(name: "before", value: String(before))) }
        return try await fabric("/api/fabric/sessions/\(session.runtimeID)/\(session.id)/turns", query: q)
    }

    public func turns(_ session: FleetSessionKey, after: Int, limit: Int = 30) async throws -> FabricTurnPage {
        let q = [URLQueryItem(name: "after", value: String(after)), URLQueryItem(name: "limit", value: String(limit))]
        return try await fabric("/api/fabric/sessions/\(session.runtimeID)/\(session.id)/turns", query: q)
    }

    public func turns(_ session: FleetSessionKey, before: Int, beforeID: String, limit: Int = 30) async throws -> FabricTurnPage {
        let q = [URLQueryItem(name: "before", value: String(before)), URLQueryItem(name: "before_id", value: beforeID),
                 URLQueryItem(name: "limit", value: String(limit))]
        do {
            return try await fabric("/api/fabric/sessions/\(session.runtimeID)/\(session.id)/turns", query: q)
        } catch let e where Self.refusesParameter(e) {
            // a server that refuses before_id: today's request
            return try await turns(session, before: before + 1, limit: limit)
        }
    }

    public func turns(_ session: FleetSessionKey, afterSeq: Int, limit: Int = 100) async throws -> FabricTurnPage? {
        let q = [URLQueryItem(name: "after_seq", value: String(afterSeq)), URLQueryItem(name: "limit", value: String(limit))]
        let page: FabricTurnPage
        do {
            page = try await fabric("/api/fabric/sessions/\(session.runtimeID)/\(session.id)/turns", query: q)
        } catch let e where Self.refusesParameter(e) {
            return nil
        }
        // a server that ignored it answers its newest page, turns without seq
        return page.isSeqPage(afterSeq: afterSeq) ? page : nil
    }

    /// A 400 (or the daemon's `{"error": …}` for a query it cannot read):
    /// this server does not take the parameter.
    static func refusesParameter(_ e: any Error) -> Bool {
        if let f = e as? FabricError { return f.status == 400 }
        switch e as? APIError {
        case .http(status: 400)?, .server?: return true
        default: return false
        }
    }

    public func turns(since: Int, limit: Int = 200) async throws -> [FabricTurn] {
        struct Body: Decodable { var turns: [FabricTurn] }
        let body: Body = try await fabric("/api/fabric/turns", query: [URLQueryItem(name: "since", value: String(since)), URLQueryItem(name: "limit", value: String(limit))])
        return body.turns
    }

    public func attention() async throws -> [FabricAttentionItem] {
        struct Body: Decodable { var items: [FabricAttentionItem] }
        let body: Body = try await fabric("/api/fabric/attention")
        return body.items
    }

    public func orders(session: FleetSessionKey?, limit: Int = 50) async throws -> [FabricOrderView] {
        struct Body: Decodable { var orders: [FabricOrderView] }
        var q = [URLQueryItem(name: "limit", value: String(limit))]
        if let session {
            q.append(URLQueryItem(name: "runtime_id", value: session.runtimeID))
            q.append(URLQueryItem(name: "session_id", value: session.id))
        }
        let body: Body = try await fabric("/api/fabric/orders", query: q)
        return body.orders
    }

    public func order(_ id: String) async throws -> FabricOrderView {
        struct Body: Decodable { var order: FabricOrderView? }
        // `{order}` or the order itself
        let data = try await fabricData("/api/fabric/orders/\(id)")
        if let wrapped = try? JSONDecoder().decode(Body.self, from: data), let order = wrapped.order { return order }
        return try Self.decodeFabric(FabricOrderView.self, data)
    }

    public func postOrder(_ body: FabricSignedBody) async throws -> FabricOrderView {
        struct Answer: Decodable { var order: FabricOrderView }
        let answer: Answer = try await fabric("/api/fabric/orders", method: "POST", body: body)
        return answer.order
    }

    public func withdrawOrder(_ id: String) async throws -> FabricOrderView {
        struct Answer: Decodable { var order: FabricOrderView }
        let answer: Answer = try await fabric("/api/fabric/orders/\(id)", method: "DELETE")
        return answer.order
    }

    public func settings() async throws -> FabricSettingsView {
        try await fabric("/api/fabric/settings")
    }

    public func putSettings(_ body: FabricSignedBody) async throws -> FabricSettingsView {
        try await fabric("/api/fabric/settings", method: "PUT", body: body)
    }

    public func projects() async throws -> [FabricProject] {
        struct Body: Decodable { var projects: [FabricProject] }
        let body: Body = try await fabric("/api/fabric/projects")
        return body.projects
    }

    public func setProject(_ name: String, textOff: Bool?, privateNotifications: Bool?) async throws -> FabricProject {
        struct Body: Encodable {
            var text_off: Bool?
            var private_notifications: Bool?
        }
        let path = "/api/fabric/projects/\(name.addingPercentEncoding(withAllowedCharacters: .urlPathAllowed.subtracting(CharacterSet(charactersIn: "/"))) ?? name)"
        return try await fabric(path, method: "PUT", body: Body(text_off: textOff, private_notifications: privateNotifications))
    }

    public func pair(runtimeID: String, pairingID: String, sealed: Data) async throws -> FabricPairingView {
        struct Body: Encodable {
            var runtime_id: String
            var pairing_id: String
            var sealed: String
        }
        return try await fabric("/api/fabric/pairing", method: "POST", body: Body(runtime_id: runtimeID, pairing_id: pairingID, sealed: sealed.base64EncodedString()))
    }

    public func pairing(_ id: String) async throws -> FabricPairingView {
        try await fabric("/api/fabric/pairing/\(id)")
    }

    public func follow(_ session: FleetSessionKey, _ on: Bool) async throws -> FleetSession {
        try await fabric("/api/fabric/sessions/\(session.runtimeID)/\(session.id)/follow", method: on ? "POST" : "DELETE")
    }

    public func dismissAttention(_ id: String) async throws {
        _ = try await fabricData("/api/fabric/attention/\(id)", method: "DELETE")
    }

    public func conversations() async throws -> [FabricConversation] {
        struct Body: Decodable { var conversations: [FabricConversation] }
        let body: Body = try await fabric("/api/fabric/conversations")
        return body.conversations
    }

    public func conversation(_ id: String, limit: Int = 50) async throws -> FabricConversationPage {
        try await fabric("/api/fabric/conversations/\(id)", query: [URLQueryItem(name: "limit", value: String(limit))])
    }

    public func continueConversation(_ id: String) async throws {
        _ = try await fabricData("/api/fabric/conversations/\(id)/continue", method: "POST")
    }

    public func stopConversation(_ id: String) async throws {
        _ = try await fabricData("/api/fabric/conversations/\(id)/stop", method: "POST")
    }

    // MARK: transport

    /// One fabric round trip. A refusal with a `code` is a `FabricError`
    /// carrying its status and code (the app words each); anything else is
    /// the usual `APIError`.
    func fabricData(_ path: String, query: [URLQueryItem] = [], method: String = "GET", body: (any Encodable)? = nil) async throws -> Data {
        var r = try await request(path, query: query, method: method)
        if let body {
            r.setValue("application/json", forHTTPHeaderField: "Content-Type")
            r.httpBody = try JSONEncoder().encode(body)
        }
        let (data, response) = try await data(for: r)
        if let http = response as? HTTPURLResponse, !(200..<300).contains(http.statusCode), http.statusCode != 401,
           let refusal = Self.fabricRefusal(data, status: http.statusCode) {
            throw refusal
        }
        try Self.check(data: data, response: response)
        return data
    }

    func fabric<T: Decodable>(_ path: String, query: [URLQueryItem] = [], method: String = "GET", body: (any Encodable)? = nil) async throws -> T {
        try Self.decodeFabric(T.self, try await fabricData(path, query: query, method: method, body: body))
    }

    static func decodeFabric<T: Decodable>(_ type: T.Type, _ data: Data) throws -> T {
        do {
            return try JSONDecoder().decode(T.self, from: data)
        } catch {
            throw APIError.decoding(String(describing: error))
        }
    }

    static func fabricRefusal(_ data: Data, status: Int) -> FabricError? {
        guard let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any], let code = obj["code"] as? String else { return nil }
        let version = (obj["version"] as? NSNumber).map { $0.uint64Value }
        return FabricError(status: status, code: code, message: obj["error"] as? String ?? code, version: version)
    }
}
