import SwiftUI

/// Shown when the server needs sign-in and the Keychain has no tokens.
struct SignInScreen: View {
    @Environment(AppModel.self) private var model
    @State private var editing = false

    var body: some View {
        SignInContent(
            serverURL: model.serverURL,
            signingIn: model.isSigningIn,
            error: model.lastError,
            onSignIn: { Task { await model.signIn() } },
            onEditServer: { editing = true }
        )
        .sheet(isPresented: $editing) { SettingsScreen() }
    }
}

struct SignInContent: View {
    let serverURL: String
    var signingIn = false
    var error: String?
    var onSignIn: () -> Void = {}
    var onEditServer: () -> Void = {}

    var body: some View {
        VStack(spacing: 0) {
            Spacer()
            VStack(spacing: 12) {
                Text("Taisce")
                    .font(.system(size: 52, weight: .semibold, design: .serif))
                    .foregroundStyle(Theme.text)
                    .accessibilityAddTraits(.isHeader)
                Text("Your library, on every device.")
                    .font(.title3)
                    .foregroundStyle(Theme.secondary)
                    .multilineTextAlignment(.center)
            }
            Spacer()
            VStack(spacing: 16) {
                if let error {
                    Text(error).font(.footnote).foregroundStyle(Theme.rose).multilineTextAlignment(.center)
                }
                Button(action: onSignIn) {
                    HStack(spacing: 8) {
                        if signingIn {
                            ProgressView().tint(Theme.ground)
                        } else {
                            Image(systemName: "person.badge.key.fill")
                        }
                        Text("Sign in with passkey")
                    }
                    .font(.headline)
                    .foregroundStyle(Theme.ground)
                    .frame(maxWidth: .infinity, minHeight: 54)
                    .background(Theme.accent, in: .rect(cornerRadius: Theme.radius, style: .continuous))
                }
                .buttonStyle(.plain)
                .disabled(signingIn)
                Button(action: onEditServer) {
                    HStack(spacing: 6) {
                        Text(serverURL).font(.footnote.monospaced())
                        Image(systemName: "pencil").font(.footnote)
                    }
                    .foregroundStyle(Theme.secondary)
                    .frame(minHeight: Theme.minTarget)
                    .contentShape(.rect)
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Server \(serverURL). Edit")
            }
            .frame(maxWidth: 420)
        }
        .padding(.horizontal, Theme.gutter + 8)
        .padding(.bottom, 24)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Theme.ground.ignoresSafeArea())
    }
}

#if DEBUG
#Preview("Sign in") {
    SignInContent(serverURL: "https://taisce.null.ie")
        .preferredColorScheme(.dark)
}

#Preview("Sign in, error, light") {
    SignInContent(serverURL: "https://taisce.null.ie", error: "Sign-in failed: the server is unreachable.")
        .preferredColorScheme(.light)
}
#endif
