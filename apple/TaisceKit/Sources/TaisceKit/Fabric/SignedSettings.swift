import Foundation

/// What an edit of the person's settings may start from (ADR 0008 §5).
/// Never the server's JSON: the newest settings as their device signed
/// them, opened and checked here. Settings this phone signed are verified
/// against its own key. Settings another of the person's devices signed are
/// believed only once the person has looked at what changed since the
/// version this phone last knew and said so (`known`): the server could
/// otherwise slip a loosened guardrail under this phone's next signature.
public enum SignedSettings: Sendable, Equatable {
    /// nothing signed yet: the built-in defaults
    case defaults(FabricSettings)
    /// signed by this device and verified, or by another and already
    /// confirmed here: edit on top of it
    case trusted(FabricSettings, payload: Data)
    /// signed by another device and not yet seen here: show `changes` and
    /// ask before signing on top of it
    case unconfirmed(FabricSettings, payload: Data, deviceID: String, changes: [String])
    /// unusable: never signed on top of
    case invalid(String)

    /// Open `view.signed`. `known` is the payload this phone last signed or
    /// confirmed (nil: none yet, compared against the defaults).
    public static func open(_ view: FabricSettingsView, me: any FabricDeviceKey, known: Data?) -> SignedSettings {
        guard let signed = view.signed else {
            return view.version == 0 ? .defaults(FabricDefaults.settings) : .invalid("The server sent settings without their signature.")
        }
        let settings: FabricSettings
        do {
            settings = try FabricSettings.decode(signed.payload)
        } catch {
            return .invalid("The saved settings can't be read.")
        }
        guard settings.version == view.version else { return .invalid("The saved settings don't match the version the server names.") }
        if signed.deviceID == me.deviceID {
            guard FabricSignature.verify(publicKey: me.publicKey, context: .settings, payload: signed.payload, signature: signed.signature) else {
                return .invalid("The saved settings claim this phone signed them, and it didn't.")
            }
            return .trusted(settings, payload: signed.payload)
        }
        if signed.payload == known { return .trusted(settings, payload: signed.payload) }
        let before = known.flatMap { try? FabricSettings.decode($0) } ?? FabricDefaults.settings
        return .unconfirmed(settings, payload: signed.payload, deviceID: signed.deviceID, changes: settings.changes(from: before))
    }

    /// What to show: the settings as signed (nil when unusable).
    public var shown: FabricSettings? {
        switch self {
        case .defaults(let s), .trusted(let s, _), .unconfirmed(let s, _, _, _): s
        case .invalid: nil
        }
    }

    /// What an edit starts from; nil when it may not start yet.
    public var base: FabricSettings? {
        switch self {
        case .defaults(let s), .trusted(let s, _): s
        case .unconfirmed, .invalid: nil
        }
    }
}

extension FabricSettings {
    /// Every guardrail, limit and fabric that differs from `before`, in words.
    public func changes(from before: FabricSettings) -> [String] {
        var out: [String] = []
        let old = Dictionary(before.guardrails.map { ($0.id, $0) }, uniquingKeysWith: { a, _ in a })
        let new = Dictionary(guardrails.map { ($0.id, $0) }, uniquingKeysWith: { a, _ in a })
        for g in before.guardrails where new[g.id] == nil { out.append("Guardrail removed: \u{201C}\(g.label)\u{201D}") }
        for g in guardrails {
            guard let o = old[g.id] else {
                out.append("Guardrail added: \u{201C}\(g.label)\u{201D}\(g.enabled ? "" : " (off)") \(Self.describe(g))")
                continue
            }
            if o.enabled != g.enabled { out.append("Guardrail \u{201C}\(g.label)\u{201D} turned \(g.enabled ? "on" : "off")") }
            if o.label != g.label { out.append("Guardrail \u{201C}\(o.label)\u{201D} renamed \u{201C}\(g.label)\u{201D}") }
            if o.pattern != g.pattern || o.builtin != g.builtin || o.tools != g.tools {
                out.append("Guardrail \u{201C}\(g.label)\u{201D} now matches \(Self.describe(g)) (was \(Self.describe(o)))")
            }
        }
        let (a, b) = ((before.limits ?? FabricLimits()).effective, (limits ?? FabricLimits()).effective)
        func limit(_ name: String, _ x: Int, _ y: Int) { if x != y { out.append("\(name): \(x) \u{2192} \(y)") } }
        limit("Messages between two agents before you're asked", a.pairMessages, b.pairMessages)
        limit("Agent turns an hour", a.agentTurnsPerHour, b.agentTurnsPerHour)
        limit("Wakes a session an hour", a.wakesPerSessionPerHour, b.wakesPerSessionPerHour)
        limit("Cold at context %", a.coldContextPct, b.coldContextPct)
        limit("Cold after idle minutes", a.coldIdleMinutes, b.coldIdleMinutes)
        let oldF = Dictionary(before.fabrics.map { ($0.id, $0) }, uniquingKeysWith: { a, _ in a })
        let newF = Set(fabrics.map(\.id))
        for f in before.fabrics where !newF.contains(f.id) { out.append("Fabric removed: \(f.name)") }
        for f in fabrics {
            if let o = oldF[f.id] {
                if o != f { out.append("Fabric changed: \(f.name)") }
            } else {
                out.append("Fabric added: \(f.name)")
            }
        }
        return out
    }

    private static func describe(_ g: FabricGuardrail) -> String {
        let what = g.builtin.isEmpty ? "/\(g.pattern)/" : "built in \(g.builtin)"
        let tools = g.tools.isEmpty ? "Bash" : g.tools.joined(separator: ", ")
        return "\(what) on \(tools)"
    }
}
