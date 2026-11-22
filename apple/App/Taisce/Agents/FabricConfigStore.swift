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
    /// where the settings this phone last signed or confirmed are kept
    @ObservationIgnored var secrets: any FabricSecretStore = KeychainSecrets()

    private(set) var view: FabricSettingsView?
    /// the newest settings, opened and checked: what an edit starts from
    private(set) var signed: SignedSettings = .defaults(FabricDefaults.settings)
    private(set) var projects: [FabricProject] = []
    private(set) var loadError: String?
    /// pairing offers this phone made, by pairing id
    private(set) var pairings: [String: FabricPairingView] = [:]
    /// what a pairing code said, by pairing id
    @ObservationIgnored private var codes: [String: PairingCode] = [:]

    private var service: (any FabricService)? { fleet?.fabricService }

    func reset() {
        view = nil
        signed = .defaults(FabricDefaults.settings)
        projects = []
        loadError = nil
        pairings = [:]
        codes = [:]
    }

    /// The settings as their device signed them (the defaults when nothing
    /// is signed yet); never the server's JSON of them.
    var settings: FabricSettings { signed.shown ?? FabricDefaults.settings }
    var fabrics: [FabricDefinition] { settings.fabrics }
    /// Changed on another device since this phone last knew: what changed,
    /// to look over and confirm before signing on top of it.
    var review: [String]? {
        if case .unconfirmed(_, _, _, let changes) = signed { return changes }
        return nil
    }
    /// The saved settings can't be used (unreadable, or a forged signature of this phone's).
    var problem: String? {
        if case .invalid(let why) = signed { return why }
        return nil
    }

    func load() async {
        guard let service else { return }
        // an answer after a sign-out or server switch is the last account's
        let asked = fleet?.generationNow
        do {
            async let s = service.settings()
            async let p = service.projects()
            let settings = try await s
            let fetched = try? await p
            guard fleet?.generationNow == asked else { return }
            apply(settings)
            projects = fetched ?? projects
            loadError = nil
        } catch {
            guard fleet?.generationNow == asked else { return }
            loadError = FleetStore.describe(error)
        }
    }

    func take(_ v: FabricSettingsView) {
        guard (view?.version ?? 0) <= v.version else { return }
        apply(v)
    }

    private var knownKey: String { "settings-known-\(fleet?.command.deviceID ?? "none")" }

    private func apply(_ v: FabricSettingsView) {
        view = v
        guard let me = try? fleet?.command.identity.current() else {
            signed = v.signed == nil && v.version == 0 ? .defaults(FabricDefaults.settings) : .invalid("This phone's key can't be read.")
            return
        }
        do {
            signed = SignedSettings.open(v, me: me, known: try secrets.data(forKey: knownKey))
        } catch {
            signed = .invalid("What this phone last saw of your settings can't be read.")
        }
    }

    /// The person looked over what another device changed: edits may build on it.
    func confirmReview() {
        guard case .unconfirmed(_, let payload, _, _) = signed, let view else { return }
        do {
            try secrets.set(payload, forKey: knownKey)
            apply(view)
        } catch {
            signed = .invalid("Couldn't keep what you confirmed: \(error.localizedDescription)")
        }
    }

    func take(_ p: FabricProject) {
        if let i = projects.firstIndex(where: { $0.name == p.name }) { projects[i] = p } else { projects.append(p) }
    }

    func take(_ p: FabricPairingView) {
        guard pairings[p.pairingID] != nil || codes[p.pairingID] != nil else { return }
        pairings[p.pairingID] = p
    }

    // MARK: settings

    /// Sign `next` as the version after the one held, and put it. Nil, or
    /// why not; a version another device saved first reloads and says so.
    func save(_ next: FabricSettings) async -> String? {
        guard let service, let command = fleet?.command else { return "Not connected." }
        switch signed {
        case .unconfirmed: return "Your settings were changed on another device. Look over what changed and confirm it first."
        case .invalid(let why): return why
        case .defaults, .trusted: break
        }
        if let bad = Self.invalidPattern(next) { return bad }
        let version = (view?.version ?? 0) + 1
        do {
            let (_, body) = try command.commander().settings(next, version: version)
            let saved = try await service.putSettings(body)
            try secrets.set(body.payload, forKey: knownKey)
            apply(saved)
            return nil
        } catch let e as FabricError where e.code == "stale_settings" {
            await load()
            return FabricWords.reason(e.code)
        } catch let e as FabricError where e.code == "not_paired_everywhere" {
            return FabricWords.notPairedEverywhere(unpairedMacs(), server: e.message)
        } catch {
            return FleetStore.describe(error)
        }
    }

    /// The Macs this phone is not paired with, by name, as the fleet has them:
    /// settings go to every Mac, so the server refuses them until each knows this phone.
    func unpairedMacs() -> [String] {
        guard let fleet else { return [] }
        return fleet.fleet.runtimes.filter { !fleet.command.isPaired(with: $0) }.map(\.name)
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
            // the code was read at the Mac itself: its key is the Mac's, kept now
            try command.trust.trust(code.runtimeID, key: code.runtimeKey)
            command.trustChanged()
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
