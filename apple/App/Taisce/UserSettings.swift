import Foundation

/// The settings this device keeps for the signed-in person on one server
/// (ADR 0004: forgotten on sign-out, so the next person starts clean): pins,
/// the last workspace, the Library's open folders. The server URL itself,
/// and push registration (unregistered on sign-out), are not the person's.
struct UserSettings {
    var defaults: UserDefaults = .standard
    let server: String

    private var suffix: String {
        let url = URL(string: server)
        return "\(url?.host() ?? "server")-\(url?.port ?? 0)"
    }

    var pinsKey: String { "pins-\(suffix)" }
    var workspaceKey: String { "workspace-\(suffix)" }
    /// `LibraryContent`'s `@AppStorage` (doc ids; not per server)
    static let libraryExpandedKey = "library.expanded"

    var keys: [String] { [pinsKey, workspaceKey, Self.libraryExpandedKey] }

    func forget() {
        for key in keys { defaults.removeObject(forKey: key) }
    }
}

/// The hosts this build claims universal links for: the Associated Domains
/// entitlement's host, mirrored into Info.plist (`TaisceAppLinkHosts`, from
/// the `TAISCE_APP_LINK_HOST` build setting) because an app can't read its
/// own entitlements. Release: taisce.null.ie; Debug: none (custom scheme).
enum AppLinks {
    static let infoKey = "TaisceAppLinkHosts"

    static func hosts(_ info: [String: Any]? = Bundle.main.infoDictionary) -> Set<String> {
        guard let raw = info?[infoKey] as? String else { return [] }
        return Set(raw.split(whereSeparator: { $0 == " " || $0 == "," }).map { $0.lowercased() }.filter { !$0.isEmpty && !$0.hasPrefix("$(") })
    }
}
