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

/// This branch's way into Books until the shell's Books tab lands (merge:
/// delete this modifier and its one line in RootView). A Debug launch with
/// `-books` shows `BooksScreen` over the app; `-books <id>` opens that book's
/// details, `-read <id>` its reader.
struct BooksDevEntry: ViewModifier {
    @Environment(AppModel.self) private var model
    @State private var shown = false
    @State private var target: Target?

    enum Target: Hashable { case books, read(String) }

    func body(content: Content) -> some View {
        content
            .onChange(of: model.treeLoaded, initial: true) { _, loaded in
                guard loaded, !shown else { return }
                let args = ProcessInfo.processInfo.arguments
                if let i = args.firstIndex(of: "-read"), i + 1 < args.count {
                    shown = true
                    target = .read(args[i + 1])
                } else if args.contains("-books") {
                    shown = true
                    target = .books
                }
            }
            .fullScreenCover(item: Binding(get: { target.map(Wrap.init) }, set: { target = $0?.t })) { w in
                switch w.t {
                case .books: NavigationStack { BooksScreen() }
                case let .read(id): BookReaderScreen(bookID: id)
                }
            }
    }

    struct Wrap: Identifiable {
        let t: Target
        var id: Target { t }
    }
}
#endif
