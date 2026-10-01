import SwiftUI

/// Shown when the server needs sign-in and the Keychain has no tokens.
struct SignInView: View {
    @Environment(AppModel.self) private var model
    @State private var showSettings = false

    var body: some View {
        NavigationStack {
            ContentUnavailableView {
                Label("Sign in to Taisce", systemImage: "person.badge.key")
            } description: {
                VStack(spacing: 8) {
                    Text(model.serverURL).font(.callout.monospaced())
                    Text("Sign in with your passkey on the server's page.")
                    if let error = model.lastError {
                        Text(error).foregroundStyle(.red).font(.footnote)
                    }
                }
            } actions: {
                Button {
                    Task { await model.signIn() }
                } label: {
                    if model.isSigningIn {
                        ProgressView()
                    } else {
                        Text("Sign in")
                    }
                }
                .buttonStyle(.borderedProminent)
                .disabled(model.isSigningIn)
            }
            .toolbar {
                ToolbarItem {
                    Button("Settings", systemImage: "gear") { showSettings = true }
                }
            }
            .sheet(isPresented: $showSettings) { SettingsView() }
        }
    }
}
