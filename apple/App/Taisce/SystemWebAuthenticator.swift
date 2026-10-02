import AuthenticationServices
import TaisceKit
import UIKit

/// `WebAuthenticator` over `ASWebAuthenticationSession`. Not ephemeral: the
/// server's sign-in page asks for a passkey, and the shared session is what
/// reaches iCloud Keychain (and keeps the user's existing Safari state).
@MainActor
final class SystemWebAuthenticator: NSObject, WebAuthenticator, ASWebAuthenticationPresentationContextProviding {
    private var session: ASWebAuthenticationSession?

    func authenticate(url: URL, callbackScheme: String) async throws -> URL {
        defer { session = nil }
        return try await withCheckedThrowingContinuation { (cont: CheckedContinuation<URL, any Error>) in
            let s = ASWebAuthenticationSession(url: url, callback: .customScheme(callbackScheme)) { callback, error in
                if let callback {
                    cont.resume(returning: callback)
                } else if let e = error as? ASWebAuthenticationSessionError, e.code == .canceledLogin {
                    cont.resume(throwing: AuthError.cancelled)
                } else {
                    cont.resume(throwing: error ?? AuthError.cancelled)
                }
            }
            s.presentationContextProvider = self
            s.prefersEphemeralWebBrowserSession = false
            session = s
            if !s.start() {
                cont.resume(throwing: AuthError.cancelled)
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
