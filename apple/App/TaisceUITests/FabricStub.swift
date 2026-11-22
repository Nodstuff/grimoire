import CryptoKit
import Foundation
import TaisceKit

/// Agent Fabric's Slice 2–5 routes for the UI tests, behind
/// `FleetStubServer`: what a hub and one runtime would answer, close enough
/// to drive every screen. It is a runtime in the one way that matters: it
/// holds a signing key, shows a pairing code for it, opens the phone's
/// sealed claim, signs its attention requests, and checks every order and
/// settings the phone posts (signature against the paired key, canonical
/// bytes, request hash against the request it signed) before taking it.
/// The JSON is ADR 0008 §11's.
final class FabricStub: @unchecked Sendable {
    let server: FleetStubServer
    let runtimeKey = SoftwareDeviceKey()
    let pairingID = UUID().uuidString.lowercased()
    let secret = Data((0..<32).map { _ in UInt8.random(in: 0...255) })
    private let lock = NSLock()

    /// paired phones: device id → public key
    private(set) var devices: [String: Data] = [:]
    /// what the phone posted and the stub took, in order
    private(set) var orders: [FabricOrder] = []
    private(set) var orderViews: [String: [String: Any]] = [:]
    private(set) var settingsPuts: [FabricSettings] = []
    private(set) var requests: [(method: String, path: String)] = []
    /// signed requests by id, for checking a request hash
    private var signedRequests: [String: Data] = [:]
    var turns: [String: [[String: Any]]] = [:]
    /// every `…/turns` request answers 503 (the server is unwell)
    var failTurns = false
    /// the `after=` each turns request carried ("" without), in order
    private(set) var turnsAfter: [String] = []
    var conversations: [[String: Any]] = []
    var messages: [String: [[String: Any]]] = [:]
    var projects: [[String: Any]] = [["name": "taisce", "text_off": false, "private_notifications": false, "sessions": 2]]
    private var settingsView: [String: Any]
    /// refuse the next order with this code (409/422), as a hub would
    var refuseNext: (status: String, code: String)?

    init(server: FleetStubServer) {
        self.server = server
        settingsView = ["version": 0, "issued_at": 0, "defaults": true, "settings": NSNull(), "signed": NSNull(), "runtimes": []]
        server.fabric = { [weak self] method, path, query, body in self?.answer(method, path, query, body) }
    }

    var runtimeID: String { StubFleet.runtime }

    /// The code the Mac would show.
    var pairingCode: String {
        func b64url(_ d: Data) -> String {
            d.base64EncodedString().replacingOccurrences(of: "+", with: "-").replacingOccurrences(of: "/", with: "_").replacingOccurrences(of: "=", with: "")
        }
        return "taisce-pair:?v=1&r=\(runtimeID)&p=\(pairingID)&k=\(b64url(runtimeKey.publicKey))&s=\(b64url(secret))&h=127.0.0.1:\(server.port)"
    }

    var lastOrder: FabricOrder? { lock.withLock { orders.last } }
    var postedOrders: [FabricOrder] { lock.withLock { orders } }
    var settingsSaved: [FabricSettings] { lock.withLock { settingsPuts } }
    var calls: [(method: String, path: String)] { lock.withLock { requests } }
    var turnsAfterSeen: [String] { lock.withLock { turnsAfter } }

    // MARK: building the fleet

    /// An attention item, signed with the runtime's key (or `forger`'s).
    func attention(_ n: Int, session: Int, address: String, kind: AttentionKind, text: String, choices: [FabricChoice] = [],
                   label: String = "", expiresIn: Int = 900, forger: (any FabricDeviceKey)? = nil) -> [String: Any] {
        let now = Int64(Date().timeIntervalSince1970)
        let request = AttentionRequest(runtimeID: runtimeID, sessionID: StubFleet.id(session), requestID: "0196f3a1-0000-7000-8000-00000000000\(n)",
                                       kind: kind, text: text, choices: choices, tool: kind == .approval ? "Bash" : "",
                                       guardrailID: label.isEmpty ? "" : "release-deploy", guardrailLabel: label,
                                       raisedAt: now, expiresAt: kind == .approval ? now + Int64(expiresIn) : 0)
        let bytes = request.encoded()
        lock.withLock { signedRequests[request.requestID] = bytes }
        let signature = (try? (forger ?? runtimeKey).sign(.attention, bytes)) ?? Data()
        return [
            "id": request.requestID, "source": "runtime", "runtime_id": runtimeID, "session_id": StubFleet.id(session), "address": address,
            "kind": kind == .approval ? "approval" : kind == .question ? "question" : kind == .dialog ? "dialog" : "tamper",
            "text": text, "choices": choices.map { ["id": $0.id, "label": $0.label] }, "tool": request.tool, "guardrail_label": label, "turn_id": "",
            "raised_at": Int(now), "expires_at": request.expiresAt == 0 ? NSNull() : Int(request.expiresAt) as Any,
            "signed": ["request": bytes.base64EncodedString(), "signature": signature.base64EncodedString()],
            "conversation_id": NSNull(), "message_id": NSNull(), "from_address": NSNull(),
        ]
    }

    /// A hub item (a wake offer, a paused pair, an ask waiting).
    static func hubItem(_ id: String, kind: String, session: Int, address: String, from: String, text: String, conversation: String) -> [String: Any] {
        [
            "id": id, "source": "hub", "runtime_id": StubFleet.runtime, "session_id": StubFleet.id(session), "address": address, "kind": kind,
            "text": text, "choices": [], "tool": "", "guardrail_label": "", "turn_id": "", "raised_at": Int(Date().timeIntervalSince1970) - 120,
            "expires_at": NSNull(), "signed": NSNull(), "conversation_id": conversation, "message_id": "m-\(id)", "from_address": from,
        ]
    }

    static func turn(_ session: Int, address: String, id: String, prompt: String, reply: String, tools: Int = 0, added: Int = 0, removed: Int = 0,
                     files: Int = 0, commits: Int = 0, ago: Int, end: String = "clean", source: String = "mac") -> [String: Any] {
        let ended = Int(Date().timeIntervalSince1970) - ago
        return [
            "runtime_id": StubFleet.runtime, "session_id": StubFleet.id(session), "address": address, "turn_id": id, "prompt": prompt, "reply": reply,
            "tool_count": tools, "outcome": ["files_changed": files, "lines_added": added, "lines_removed": removed, "commits": commits,
                                             "pull_request_url": NSNull(), "pull_request_number": NSNull()],
            "started": ended - 90, "ended": ended, "truncated": false, "context_used_pct": 44, "end": end, "source": source, "order_id": NSNull(), "text_off": false,
        ]
    }

    static func party(_ n: Int, _ address: String) -> [String: Any] {
        ["runtime_id": StubFleet.runtime, "session_id": StubFleet.id(n), "address": address]
    }

    // MARK: the routes

    private func answer(_ method: String, _ path: String, _ query: [String: String], _ body: Data) -> (status: String, json: String)? {
        lock.withLock { requests.append((method, path)) }
        let parts = path.split(separator: "/").map(String.init)
        let json = { (v: Any) in FleetStubServer.json(v) }
        func refuse(_ status: String, _ code: String) -> (String, String) { (status, json(["error": code, "code": code])) }
        switch (method, parts.count, parts.dropFirst(2).first ?? "") {
        case ("GET", 6, "sessions") where parts[5] == "turns":
            lock.withLock { turnsAfter.append(query["after"] ?? "") }
            if failTurns { return refuse("503 Service Unavailable", "unavailable") }
            let ended = { (t: [String: Any]) in t["ended"] as? Int ?? 0 }
            // `after=`: the turns ended since, oldest first; else newest first,
            // `before=` exclusive; at most `limit`, with `more` past it
            let all = turns[parts[4]] ?? []
            let limit = query["limit"].flatMap(Int.init) ?? all.count
            if let after = query["after"].flatMap(Int.init) {
                let list = all.filter { ended($0) > after }.sorted { ended($0) < ended($1) }
                return ("200 OK", json(["turns": Array(list.prefix(limit)), "more": list.count > limit]))
            }
            let before = query["before"].flatMap(Int.init) ?? .max
            let list = all.filter { ended($0) < before }.sorted { ended($0) > ended($1) }
            return ("200 OK", json(["turns": Array(list.prefix(limit)), "more": list.count > limit]))
        case ("POST", 6, "sessions") where parts[5] == "follow", ("DELETE", 6, "sessions") where parts[5] == "follow":
            server.update(session: parts[4]) { $0["following"] = method == "POST" }
            let s = (server.fleetJSON["sessions"] as? [[String: Any]] ?? []).first { $0["id"] as? String == parts[4] } ?? [:]
            return ("200 OK", json(s))
        case ("GET", 3, "attention"):
            return ("200 OK", json(["items": server.fleetJSON["attention"] ?? []]))
        case ("DELETE", 4, "attention"):
            clearAttention(parts[3], resolution: "dismissed")
            return ("204 No Content", "")
        case ("GET", 3, "orders"):
            let list = lock.withLock { Array(orderViews.values) }.filter { query["session_id"] == nil || $0["session_id"] as? String == query["session_id"] }
            return ("200 OK", json(["orders": list]))
        case ("POST", 3, "orders"):
            return takeOrder(body)
        case ("DELETE", 4, "orders"):
            guard var o = lock.withLock({ orderViews[parts[3]] }) else { return refuse("404 Not Found", "not_found") }
            guard o["state"] as? String == "pending" else { return refuse("409 Conflict", "already_sent") }
            o["state"] = "withdrawn"
            lock.withLock { orderViews[parts[3]] = o }
            return ("200 OK", json(["order": o]))
        case ("GET", 3, "settings"):
            return ("200 OK", json(lock.withLock { settingsView }))
        case ("PUT", 3, "settings"):
            return takeSettings(body)
        case ("GET", 3, "projects"):
            return ("200 OK", json(["projects": projects]))
        case ("PUT", 4, "projects"):
            let change = (try? JSONSerialization.jsonObject(with: body)) as? [String: Any] ?? [:]
            guard let i = projects.firstIndex(where: { $0["name"] as? String == parts[3] }) else { return refuse("404 Not Found", "not_found") }
            for (k, v) in change where !(v is NSNull) { projects[i][k] = v }
            server.broadcast("project", projects[i])
            return ("200 OK", json(projects[i]))
        case ("POST", 3, "pairing"):
            return takePairing(body)
        case ("GET", 4, "pairing"):
            let paired = lock.withLock { !devices.isEmpty }
            return ("200 OK", json(["pairing_id": parts[3], "runtime_id": runtimeID, "state": paired ? "paired" : "sent", "device_id": NSNull(), "detail": NSNull()]))
        case ("GET", 3, "conversations"):
            return ("200 OK", json(["conversations": conversations]))
        case ("GET", 4, "conversations"):
            guard let c = conversations.first(where: { $0["id"] as? String == parts[3] }) else { return refuse("404 Not Found", "not_found") }
            return ("200 OK", json(["conversation": c, "messages": messages[parts[3]] ?? []]))
        case ("POST", 5, "conversations"):
            guard let i = conversations.firstIndex(where: { $0["id"] as? String == parts[3] }) else { return refuse("404 Not Found", "not_found") }
            conversations[i]["paused"] = false
            for item in (server.fleetJSON["attention"] as? [[String: Any]] ?? []) where item["conversation_id"] as? String == parts[3] {
                clearAttention(item["id"] as? String ?? "", resolution: parts[4] == "continue" ? "continued" : "stopped")
            }
            server.broadcast("conversation", conversations[i])
            return ("200 OK", json(conversations[i]))
        default:
            return nil
        }
    }

    func clearAttention(_ id: String, resolution: String) {
        server.mutate { f in f["attention"] = (f["attention"] as? [[String: Any]] ?? []).filter { $0["id"] as? String != id } }
        server.broadcast("attention_cleared", ["id": id, "runtime_id": runtimeID, "session_id": "", "resolution": resolution])
    }

    /// The hub's checks, then the runtime's: a paired device's valid
    /// signature over canonical bytes; an approval's hash naming the bytes
    /// this runtime signed.
    private func takeOrder(_ body: Data) -> (status: String, json: String) {
        let refuse = { (status: String, code: String) in (status, FleetStubServer.json(["error": code, "code": code])) }
        guard let b = (try? JSONSerialization.jsonObject(with: body)) as? [String: String],
              let payload = b["payload"].flatMap({ Data(base64Encoded: $0) }), let sig = b["signature"].flatMap({ Data(base64Encoded: $0) }),
              let device = b["device_id"] else { return refuse("400 Bad Request", "malformed") }
        guard let key = lock.withLock({ devices[device] }) else { return refuse("422 Unprocessable Entity", "unknown_device") }
        guard FabricSignature.verify(publicKey: key, context: .order, payload: payload, signature: sig) else { return refuse("422 Unprocessable Entity", "bad_signature") }
        guard FabricOrder.isCanonical(payload), let order = try? FabricOrder.decode(payload) else { return refuse("400 Bad Request", "non_canonical") }
        if let r = refuseNext { refuseNext = nil; return refuse(r.status, r.code) }
        var view: [String: Any] = [
            "id": order.orderID, "runtime_id": order.runtimeID, "device_id": device, "counter": order.counter, "issued_at": order.issuedAt,
            "expires_at": order.expiresAt, "created_at": Int(Date().timeIntervalSince1970), "updated_at": Int(Date().timeIntervalSince1970),
            "state": "pending", "reason": NSNull(), "detail": NSNull(), "queued": false, "order_ids": [], "after_turn": "", "request_id": "", "choice_id": "",
        ]
        switch order.what {
        case .hold(let on):
            view["kind"] = "hold"; view["hold"] = on; view["verb"] = NSNull(); view["session_id"] = NSNull(); view["text"] = NSNull()
            server.updateRuntime { $0["held"] = on; $0["held_since"] = on ? Int(Date().timeIntervalSince1970) : NSNull() }
        case .command(let c):
            view["kind"] = "command"; view["verb"] = FabricVerb(rawValue: c.verb.rawValue)?.name; view["session_id"] = c.sessionID
            view["text"] = c.text.isEmpty ? NSNull() : c.text; view["after_turn"] = c.afterTurn; view["queued"] = c.queued
            view["request_id"] = c.requestID; view["choice_id"] = c.choiceID; view["order_ids"] = c.orderIDs
            if !c.requestID.isEmpty {
                guard let signed = lock.withLock({ signedRequests[c.requestID] }), FabricSignature.requestHash(signed) == c.requestHash else {
                    return refuse("422 Unprocessable Entity", "stale_request")
                }
                clearAttention(c.requestID, resolution: c.verb == .approve ? "approved" : c.verb == .deny ? "denied" : "answered")
            }
            if c.verb == .cancel {
                server.update(session: c.sessionID) { s in s["queue"] = (s["queue"] as? [String] ?? []).filter { !c.orderIDs.contains($0) } }
            }
            if c.verb == .send, c.queued {
                server.update(session: c.sessionID) { s in s["queue"] = (s["queue"] as? [String] ?? []) + [order.orderID] }
                view["state"] = "queued"
            }
        }
        lock.withLock {
            orders.append(order)
            orderViews[order.orderID] = view
        }
        server.broadcast("order", view)
        return ("202 Accepted", FleetStubServer.json(["order": view]))
    }

    /// Add an order the phone did not send (a queued prompt from before), with its text.
    func seedOrder(_ id: String, session: Int, text: String, state: String = "queued") {
        let view: [String: Any] = [
            "id": id, "runtime_id": runtimeID, "session_id": StubFleet.id(session), "device_id": "earlier", "counter": 1, "kind": "command", "verb": "send",
            "hold": NSNull(), "text": text, "after_turn": "", "queued": true, "request_id": "", "choice_id": "", "order_ids": [], "issued_at": 1, "expires_at": 2,
            "created_at": Int(Date().timeIntervalSince1970) - 60, "updated_at": 1, "state": state, "reason": NSNull(), "detail": NSNull(),
        ]
        lock.withLock { orderViews[id] = view }
    }

    private func takeSettings(_ body: Data) -> (status: String, json: String) {
        let refuse = { (status: String, code: String) in (status, FleetStubServer.json(["error": code, "code": code])) }
        guard let b = (try? JSONSerialization.jsonObject(with: body)) as? [String: String],
              let payload = b["payload"].flatMap({ Data(base64Encoded: $0) }), let sig = b["signature"].flatMap({ Data(base64Encoded: $0) }),
              let device = b["device_id"], let key = lock.withLock({ devices[device] }) else { return refuse("422 Unprocessable Entity", "unknown_device") }
        guard FabricSignature.verify(publicKey: key, context: .settings, payload: payload, signature: sig) else { return refuse("422 Unprocessable Entity", "bad_signature") }
        let version = lock.withLock { settingsView["version"] as? Int ?? 0 }
        guard let settings = try? FabricSettings.decode(payload), FabricSettings.isCanonical(payload) else { return refuse("400 Bad Request", "non_canonical") }
        guard settings.version == UInt64(version + 1) else {
            return ("409 Conflict", FleetStubServer.json(["error": "stale", "code": "stale_settings", "version": version]))
        }
        let decoded = (settings: settings, json: (try? JSONSerialization.jsonObject(with: JSONEncoder().encode(settings))) ?? [:])
        lock.withLock {
            settingsPuts.append(decoded.settings)
            settingsView = ["version": version + 1, "issued_at": Int(Date().timeIntervalSince1970), "defaults": false, "settings": decoded.json,
                            "signed": ["payload": payload.base64EncodedString(), "device_id": device, "signature": sig.base64EncodedString()], "runtimes": []]
        }
        let view = lock.withLock { settingsView }
        server.broadcast("settings", view)
        return ("200 OK", FleetStubServer.json(view))
    }

    private func takePairing(_ body: Data) -> (status: String, json: String) {
        let refuse = { (status: String, code: String) in (status, FleetStubServer.json(["error": code, "code": code])) }
        guard let b = (try? JSONSerialization.jsonObject(with: body)) as? [String: String], b["pairing_id"] == pairingID,
              let sealed = b["sealed"].flatMap({ Data(base64Encoded: $0) }) else { return refuse("400 Bad Request", "malformed") }
        // the runtime's side: open the seal with the code's secret and check the claim
        let key = HKDF<SHA256>.deriveKey(inputKeyMaterial: SymmetricKey(data: secret), salt: Data(pairingID.utf8), info: Data("taisce/pairing/v1".utf8), outputByteCount: 32)
        guard let box = try? AES.GCM.SealedBox(combined: sealed), let plain = try? AES.GCM.open(box, using: key, authenticating: Data(runtimeID.utf8)),
              let seal = try? PairingSeal.decode(plain), let claim = try? PairingClaim.decode(seal.claim),
              claim.runtimeID == runtimeID, claim.pairingID == pairingID,
              FabricSignature.verify(publicKey: claim.publicKey, context: .pairing, payload: seal.claim, signature: seal.signature)
        else { return refuse("422 Unprocessable Entity", "invalid") }
        let pub = claim.publicKey
        let deviceID = FabricSignature.deviceID(publicKey: pub)
        lock.withLock { devices[deviceID] = pub }
        let view: [String: Any] = ["pairing_id": pairingID, "runtime_id": runtimeID, "state": "sent", "device_id": NSNull(), "detail": NSNull()]
        // the person confirms at the Mac a moment later
        DispatchQueue.global().asyncAfter(deadline: .now() + 1.5) { [self] in
            server.updateRuntime { r in
                r["devices"] = [["device_id": deviceID, "fingerprint": FabricSignature.fingerprint(deviceID: deviceID), "name": "phone",
                                 "public_key": pub.base64EncodedString(), "paired_at": Int(Date().timeIntervalSince1970), "counter": 0]]
                r["public_key"] = runtimeKey.publicKey.base64EncodedString()
            }
            server.broadcast("pairing", ["pairing_id": pairingID, "runtime_id": runtimeID, "state": "paired", "device_id": deviceID, "detail": NSNull()])
        }
        return ("202 Accepted", FleetStubServer.json(view))
    }

}
