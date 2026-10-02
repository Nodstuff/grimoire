import AuthenticationServices
import Foundation
import Testing
import TaisceKit
@testable import Taisce

/// The Mac delivers ASWebAuthenticationSession's completion on an XPC queue;
/// calling it off the main thread must resume, not trap.
struct WebAuthenticatorTests {
    @Test func completionResumesFromABackgroundQueue() async throws {
        let url = URL(string: "ie.null.taisce:/oauth/callback?code=x")!
        let got: URL = try await withCheckedThrowingContinuation { cont in
            let handler = SystemWebAuthenticator.completion(cont)
            DispatchQueue.global().async { handler(url, nil) }
        }
        #expect(got == url)
    }

    @Test func cancelFromABackgroundQueueIsCancelled() async {
        await #expect(throws: AuthError.self) {
            let _: URL = try await withCheckedThrowingContinuation { cont in
                let handler = SystemWebAuthenticator.completion(cont)
                let err = ASWebAuthenticationSessionError(.canceledLogin)
                DispatchQueue.global().async { handler(nil, err) }
            }
        }
    }
}
