import SwiftUI
import TaisceKit

extension AppModel {
    /// The sign-in sheet: the system's (passkeys), or in a Debug UI-test run
    /// with `TAISCE_UI_SIGNIN_RELAY` set, a relay on the Mac that plays the
    /// passkey with the daemon's softpasskey example (a simulator can't).
    static func webAuthenticator() -> any WebAuthenticator {
        #if DEBUG
        if let relay = SignInRelay.configured { return relay }
        #endif
        return SystemWebAuthenticator()
    }
}

#if DEBUG
/// Debug UI tests only: hands the authorize URL to a loopback relay
/// (`TAISCE_UI_SIGNIN_RELAY`, e.g. http://127.0.0.1:7587/signin) that
/// enrolls a software passkey on a SERVER-mode scratch daemon, signs in
/// with it and answers the redirect. The OAuth exchange that follows is the
/// app's own. Never compiled into Release; never used without `TAISCE_UI_TEST=1`.
struct SignInRelay: WebAuthenticator {
    let relay: URL

    static var configured: SignInRelay? {
        let env = ProcessInfo.processInfo.environment
        guard env["TAISCE_UI_TEST"] == "1", let raw = env["TAISCE_UI_SIGNIN_RELAY"], let url = URL(string: raw),
              ["127.0.0.1", "localhost", "::1"].contains(url.host() ?? "")
        else { return nil }
        return SignInRelay(relay: url)
    }

    func authenticate(url: URL, callback: OAuthCallback) async throws -> URL {
        var r = URLRequest(url: relay)
        r.httpMethod = "POST"
        r.timeoutInterval = 60
        r.httpBody = Data(url.absoluteString.utf8)
        let (data, response) = try await URLSession.shared.data(for: r)
        guard (response as? HTTPURLResponse)?.statusCode == 200,
              let text = String(data: data, encoding: .utf8)?.trimmingCharacters(in: .whitespacesAndNewlines),
              let redirect = URL(string: text)
        else { throw AuthError.cancelled }
        return redirect
    }
}

/// A Debug UI-test run with the sign-in relay signs in by itself (no sheet
/// to tap through). Books opens with `-tab books`, a reader with `-readBook <id>`
/// (`Router.applyLaunchArguments`).
struct SignInRelayAutoStart: ViewModifier {
    @Environment(AppModel.self) private var model

    func body(content: Content) -> some View {
        content.onChange(of: model.needsSignIn, initial: true) { _, needs in
            if needs, SignInRelay.configured != nil { Task { await model.signIn() } }
        }
    }
}
#endif
