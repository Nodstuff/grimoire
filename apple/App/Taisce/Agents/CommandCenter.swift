import Foundation
import Observation
import TaisceKit

/// A reply held for five seconds before it is signed and posted: the
/// undo window (ADR 0008 §5).
struct PendingSend: Identifiable, Equatable, Sendable {
    /// the order id it will carry
    let id: String
    let session: FleetSessionKey
    let address: String
    let text: String
    /// queue it behind what is running rather than refuse it if the turn moved on
    let queued: Bool
    /// the turn the person was reading
    let afterTurn: String
    let deadline: Date
}

/// Something worth saying once, at the bottom of the screen.
struct FabricNotice: Identifiable, Equatable {
    let id = UUID()
    let text: String
    var isError = true
}

/// Everything the phone commands (Slice 3): each order built and signed with
/// this device's key and posted; sends held five seconds first; approvals
/// verified against the Mac's own key and confirmed with Face ID; the
/// orders' fates followed from the stream. Owned by `FleetStore`.
@MainActor @Observable
final class CommandCenter {
    @ObservationIgnored weak var fleet: FleetStore?
    /// tests: a key and stores of their own
    @ObservationIgnored var identity = FabricDeviceIdentity()
    @ObservationIgnored var counters: any FabricCounterStore = DefaultsCounterStore()
    @ObservationIgnored var trust: any FabricTrustStore = KeychainTrustStore()
    @ObservationIgnored var ownerCheck: any OwnerCheck = DeviceOwnerCheck()
    /// how long a send waits before it goes (tests: none)
    @ObservationIgnored var undoWindow: Duration = .seconds(5)

    /// sends inside their undo window
    private(set) var pending: [PendingSend] = []
    /// orders this phone has seen, by id (the stream's and the routes')
    private(set) var orders: [String: FabricOrderView] = [:]
    /// what went wrong last, for the screen in front
    var notice: FabricNotice?
    /// orders on their way (a spinner on the button that sent them)
    private(set) var inFlight: Set<String> = []

    @ObservationIgnored private var timers: [String: Task<Void, Never>] = [:]

    private var service: (any FabricService)? { fleet?.fabricService }

    func reset() {
        timers.values.forEach { $0.cancel() }
        timers = [:]
        pending = []
        orders = [:]
        notice = nil
        inFlight = []
    }

    /// This device's key, made on first use.
    func commander() throws -> FabricCommander {
        FabricCommander(key: try identity.current(), counters: counters)
    }

    var deviceID: String? { identity.existing()?.deviceID }

    /// This phone is paired with the runtime (it is in the devices the Mac reports).
    func isPaired(with runtime: FleetRuntime) -> Bool {
        guard let id = deviceID else { return false }
        return runtime.isPaired(id)
    }

    // MARK: the stream

    func take(_ order: FabricOrderView) {
        let before = orders[order.id]
        orders[order.id] = order
        // a refusal of something this phone sent, said once where it happened
        if before?.state != order.state, order.state == .refused || order.state == .failed,
           order.deviceID == deviceID, order.reason != "cancelled" {
            let address = order.sessionID.flatMap { s in fleet?.fleet.sessions.first { $0.sessionID == s }?.address }
            let words = FabricWords.state(order)
            notice = FabricNotice(text: address.map { "\($0): \(words)" } ?? words)
        }
    }

    /// The audit trail of one session (its queued prompts' text among it).
    func loadOrders(for key: FleetSessionKey) async {
        guard let service, let list = try? await service.orders(session: key, limit: 50) else { return }
        for o in list { orders[o.id] = o }
    }

    func orders(for key: FleetSessionKey) -> [FabricOrderView] {
        orders.values.filter { $0.runtimeID == key.runtimeID && $0.sessionID == key.id }.sorted { $0.createdAt > $1.createdAt }
    }

    // MARK: sending, with undo

    /// Hold a reply for the undo window, then sign and post it. Nil, or why
    /// it cannot be sent at all.
    @discardableResult
    func send(_ text: String, to session: FleetSession, queued: Bool, now: Date = .now) -> String? {
        do { try FabricCommander.check(text: text) } catch let p as FabricOrderProblem { return FabricWords.problem(p) } catch { return error.localizedDescription }
        let p = PendingSend(id: UUID().uuidString.lowercased(), session: session.key, address: session.address, text: text,
                            queued: queued, afterTurn: session.turnID, deadline: now.addingTimeInterval(undoWindow.seconds))
        pending.append(p)
        let wait = undoWindow
        timers[p.id] = Task { [weak self] in
            if wait > .zero { try? await Task.sleep(for: wait) }
            guard !Task.isCancelled else { return }
            await self?.post(p.id)
        }
        return nil
    }

    /// Take a held send back before it goes. Its text, for the composer.
    func undo(_ id: String) -> String? {
        timers.removeValue(forKey: id)?.cancel()
        guard let i = pending.firstIndex(where: { $0.id == id }) else { return nil }
        return pending.remove(at: i).text
    }

    /// Send every held reply now (the app is leaving the front).
    func flush() async {
        for p in pending {
            timers.removeValue(forKey: p.id)?.cancel()
            await post(p.id)
        }
    }

    private func post(_ id: String) async {
        timers[id] = nil
        guard let i = pending.firstIndex(where: { $0.id == id }) else { return }
        let p = pending.remove(at: i)
        let command = FabricCommand(sessionID: p.session.id, verb: .send, text: p.text, afterTurn: p.afterTurn, queued: p.queued)
        if let failure = await order(.command(command), runtimeID: p.session.runtimeID, orderID: p.id) {
            notice = FabricNotice(text: "\(p.address): \(failure)")
        }
    }

    /// Withdraw an order still waiting at the server (no signature needed:
    /// the hub may always drop traffic). One already sent cannot be.
    func withdraw(_ orderID: String) async -> String? {
        guard let service else { return "Not connected." }
        do {
            take(try await service.withdrawOrder(orderID))
            return nil
        } catch {
            return FleetStore.describe(error)
        }
    }

    // MARK: orders

    /// Build, sign and post one order to one runtime. Nil, or why not.
    @discardableResult
    func order(_ what: FabricOrder.What, runtimeID: String, lifetime: TimeInterval = FabricCommander.replyLifetime,
               orderID: String = UUID().uuidString.lowercased()) async -> String? {
        guard let service else { return "Not connected." }
        guard let runtime = fleet?.fleet.runtime(runtimeID) else { return "That Mac isn't in your fleet." }
        inFlight.insert(orderID)
        defer { inFlight.remove(orderID) }
        do {
            let (_, body) = try commander().order(what, runtime: runtime, lifetime: lifetime, orderID: orderID)
            take(try await service.postOrder(body))
            return nil
        } catch {
            return FleetStore.describe(error)
        }
    }

    /// One command to a session.
    @discardableResult
    func command(_ verb: FabricVerb, _ session: FleetSessionKey, text: String = "", orderIDs: [String] = []) async -> String? {
        await order(.command(FabricCommand(sessionID: session.id, verb: verb, text: text, orderIDs: orderIDs)), runtimeID: session.runtimeID)
    }

    /// Hold all (or release it) on every runtime: one order to each.
    /// Nil, or what went wrong where.
    func hold(_ on: Bool) async -> String? {
        guard let runtimes = fleet?.fleet.runtimes, !runtimes.isEmpty else { return "No Mac is enrolled." }
        var failures: [String] = []
        for r in runtimes {
            if let f = await order(.hold(on), runtimeID: r.id) { failures.append("\(r.name): \(f)") }
        }
        return failures.isEmpty ? nil : failures.joined(separator: "\n")
    }

    // MARK: attention

    /// The item, verified against the key from the Mac's pairing code.
    func verify(_ item: FabricAttentionItem) -> Result<VerifiedAttention, FabricOrderProblem> {
        do {
            return .success(try VerifiedAttention.verify(item, trust: trust))
        } catch let p as FabricOrderProblem {
            return .failure(p)
        } catch {
            return .failure(.unverified(error.localizedDescription))
        }
    }

    /// Allow the exact request shown, once, after Face ID. Nil, or why not
    /// ("" when the person cancelled Face ID).
    func approve(_ v: VerifiedAttention) async -> String? {
        guard await ownerCheck.confirm("Allow \(v.item.address) to run this once") else { return "" }
        return await order(.command(v.approve()), runtimeID: v.item.runtimeID, lifetime: v.lifetime(now: .now))
    }

    func deny(_ v: VerifiedAttention, because reason: String = "") async -> String? {
        await order(.command(v.deny(because: reason)), runtimeID: v.item.runtimeID, lifetime: v.lifetime(now: .now))
    }

    func answer(_ v: VerifiedAttention, choice: FabricChoice) async -> String? {
        await order(.command(v.answer(choice: choice)), runtimeID: v.item.runtimeID)
    }

    func answer(_ v: VerifiedAttention, words: String) async -> String? {
        do { try FabricCommander.check(text: words) } catch let p as FabricOrderProblem { return FabricWords.problem(p) } catch { return error.localizedDescription }
        return await order(.command(v.answer(words: words)), runtimeID: v.item.runtimeID)
    }
}

extension Duration {
    var seconds: TimeInterval {
        let (s, atto) = components
        return TimeInterval(s) + TimeInterval(atto) / 1e18
    }
}
