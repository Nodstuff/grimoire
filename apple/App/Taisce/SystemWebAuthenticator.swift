import AuthenticationServices
import TaisceKit
import UIKit

/// `WebAuthenticator` over `ASWebAuthenticationSession`. Not ephemeral: the
/// server's sign-in page asks for a passkey, and the shared session is what
/// reaches iCloud Keychain (and keeps the user's existing Safari state).
@MainActor
final class SystemWebAuthenticator: NSObject, WebAuthenticator, ASWebAuthenticationPresentationContextProviding {
    private var session: ASWebAuthenticationSession?

    func authenticate(url: URL, callback: OAuthCallback) async throws -> URL {
        defer { session = nil }
        return try await withCheckedThrowingContinuation { (cont: CheckedContinuation<URL, any Error>) in
            let s = ASWebAuthenticationSession(url: url, callback: Self.sessionCallback(callback), completionHandler: Self.completion(cont))
            s.presentationContextProvider = self
            s.prefersEphemeralWebBrowserSession = false
            session = s
            if !s.start() {
                cont.resume(throwing: AuthError.cancelled)
            }
        }
    }

    /// The universal link (`.https`, iOS 17.4 / macOS 14.4, below this
    /// app's targets) when sign-in chose it: only this app can claim it,
    /// through the server's `apple-app-site-association` file and the
    /// Associated Domains entitlement. Else the custom scheme, as before.
    nonisolated static func sessionCallback(_ callback: OAuthCallback) -> ASWebAuthenticationSession.Callback {
        switch callback {
        case let .https(host, path): .https(host: host, path: path)
        case let .customScheme(scheme): .customScheme(scheme)
        }
    }

    /// The session's completion handler. It must not be main-actor isolated:
    /// on the Mac, AuthenticationServices calls it on an XPC queue, and a
    /// closure inferred `@MainActor` from this class traps there
    /// (`_dispatch_assert_queue_fail`) right after a successful sign-in.
    nonisolated static func completion(_ cont: CheckedContinuation<URL, any Error>) -> @Sendable (URL?, (any Error)?) -> Void {
        { callback, error in
            if let callback {
                cont.resume(returning: callback)
            } else if let e = error as? ASWebAuthenticationSessionError, e.code == .canceledLogin {
                cont.resume(throwing: AuthError.cancelled)
            } else {
                cont.resume(throwing: error ?? AuthError.cancelled)
            }
        }
    }

    func presentationAnchor(for session: ASWebAuthenticationSession) -> ASPresentationAnchor {
        // the window in front first: a Mac can have the app's window behind another app's
        let scenes = UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }
            .sorted { ($0.activationState == .foregroundActive ? 0 : 1) < ($1.activationState == .foregroundActive ? 0 : 1) }
        if let key = scenes.lazy.compactMap(\.keyWindow).first { return key }
        // sign-in starts from a button, so a scene exists
        guard let scene = scenes.first else { preconditionFailure("sign-in with no window scene") }
        return UIWindow(windowScene: scene)
    }
}
