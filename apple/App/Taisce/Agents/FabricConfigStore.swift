import Foundation
import Observation
import TaisceKit

/// The person's standing configuration (guardrails, limits, fabrics: one
/// signed, versioned Settings), each project's Text off and private
/// notifications, and pairing this phone with a Mac.
@MainActor @Observable
final class FabricConfigStore {
    @ObservationIgnored weak var fleet: FleetStore?
    /// tests: no waiting between pairing polls
    @ObservationIgnored var pairingPoll: Duration = .seconds(2)

    private(set) var view: FabricSettingsView?
    private(set) var projects: [FabricProject] = []
    private(set) var loadError: String?
    /// pairing offers this phone made, by pairing id
    private(set) var pairings: [String: FabricPairingView] = [:]
    /// what a pairing code said, by pairing id (the runtime's key is trusted once paired)
    @ObservationIgnored private var codes: [String: PairingCode] = [:]

    private var service: (any FabricService)? { fleet?.fabricService }

    func reset() {
        view = nil
        projects = []
        loadError = nil
        pairings = [:]
        codes = [:]
    }

    /// The settings in force, or the defaults when nothing is signed yet.
    var settings: FabricSettings { view?.settings ?? FabricDefaults.settings }
    var fabrics: [FabricDefinition] { settings.fabrics }

    func load() async {
        guard let service else { return }
        do {
            async let s = service.settings()
            async let p = service.projects()
            view = try await s
            projects = (try? await p) ?? projects
            loadError = nil
        } catch {
            loadError = FleetStore.describe(error)
        }
    }

    func take(_ v: FabricSettingsView) {
        guard (view?.version ?? 0) <= v.version else { return }
        view = v
    }

    func take(_ p: FabricProject) {
        if let i = projects.firstIndex(where: { $0.name == p.name }) { projects[i] = p } else { projects.append(p) }
    }

    func take(_ p: FabricPairingView) {
        guard pairings[p.pairingID] != nil || codes[p.pairingID] != nil else { return }
        pairings[p.pairingID] = p
        if p.state == .paired, let code = codes[p.pairingID] {
            fleet?.command.trust.trust(code.runtimeID, key: code.runtimeKey)
        }
    }

    // MARK: settings

    /// Sign `next` as the version after the one held, and put it. Nil, or
    /// why not; a version another device saved first reloads and says so.
    func save(_ next: FabricSettings) async -> String? {
        guard let service, let command = fleet?.command else { return "Not connected." }
        if let bad = Self.invalidPattern(next) { return bad }
        let version = (view?.version ?? 0) + 1
        do {
            let (_, body) = try command.commander().settings(next, version: version)
            view = try await service.putSettings(body)
            return nil
        } catch let e as FabricError where e.code == "stale_settings" {
            await load()
            return FabricWords.reason(e.code)
        } catch {
            return FleetStore.describe(error)
        }
    }

    /// A pattern NSRegularExpression cannot compile (the runtime would
    /// refuse the whole Settings: guardrails fail closed), said before signing.
    static func invalidPattern(_ s: FabricSettings) -> String? {
        for g in s.guardrails where !g.pattern.isEmpty {
            if (try? NSRegularExpression(pattern: g.pattern)) == nil { return "\u{201C}\(g.label)\u{201D}: the pattern doesn't compile." }
        }
        var names = Set<String>()
        for f in s.fabrics {
            let name = f.name.trimmingCharacters(in: .whitespaces)
            if name.isEmpty { return "A fabric needs a name." }
            if name.contains("/") || name.contains(":") { return "\u{201C}\(name)\u{201D}: a fabric's name can't contain / or :." }
            if !names.insert(name.lowercased()).inserted { return "Two fabrics are called \u{201C}\(name)\u{201D}." }
        }
        return nil
    }

    // MARK: projects

    func setProject(_ name: String, textOff: Bool? = nil, privateNotifications: Bool? = nil) async -> String? {
        guard let service else { return "Not connected." }
        do {
            take(try await service.setProject(name, textOff: textOff, privateNotifications: privateNotifications))
            return nil
        } catch {
            return FleetStore.describe(error)
        }
    }

    // MARK: pairing

    /// Read a code the Mac shows, check it names this server, seal this
    /// device's key for that runtime and post it. The pairing id, or why not.
    func pair(code text: String, deviceName: String, serverHost: String?) async -> Result<String, PairingFailure> {
        guard let service, let command = fleet?.command else { return .failure(.words("Not connected.")) }
        let code: PairingCode
        do { code = try PairingCode(text) } catch { return .failure(.words("That isn't a pairing code from a Mac's runtime.")) }
        if let serverHost, !Self.sameHost(code.host, serverHost) {
            return .failure(.words("That Mac reports to \(code.host), and this app is signed in to \(serverHost)."))
        }
        do {
            let sealed = try code.seal(with: try command.identity.current(), deviceName: deviceName)
            codes[code.pairingID] = code
            let view = try await service.pair(runtimeID: code.runtimeID, pairingID: code.pairingID, sealed: sealed)
            take(view)
            return .success(code.pairingID)
        } catch {
            codes[code.pairingID] = nil
            return .failure(.words(FleetStore.describe(error)))
        }
    }

    enum PairingFailure: Error, Equatable { case words(String) }

    /// The code's own runtime fingerprint (to compare with the Mac's).
    func code(for pairingID: String) -> PairingCode? { codes[pairingID] }

    /// Ask until the Mac answers (the stream usually says first).
    func watch(_ pairingID: String) async {
        guard let service else { return }
        while !Task.isCancelled, !(pairings[pairingID]?.state.isFinal ?? false) {
            try? await Task.sleep(for: pairingPoll)
            if let v = try? await service.pairing(pairingID) { take(v) }
        }
    }

    static func sameHost(_ a: String, _ b: String) -> Bool {
        func bare(_ h: String) -> String {
            var s = h.lowercased()
            if let u = URL(string: s), let host = u.host() { s = host + (u.port.map { ":\($0)" } ?? "") }
            return s.trimmingCharacters(in: CharacterSet(charactersIn: "/"))
        }
        let (x, y) = (bare(a), bare(b))
        // a code may name the host without the port the app reaches it on
        return x == y || x.split(separator: ":").first == y.split(separator: ":").first
    }
}
