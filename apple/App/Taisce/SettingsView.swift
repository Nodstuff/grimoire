import SwiftUI

struct SettingsView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    @State private var url = ""

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    TextField("Server URL", text: $url)
                        .textContentType(.URL)
                        .keyboardType(.URL)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                } header: {
                    Text("Server")
                } footer: {
                    Text("HTTPS servers ask you to sign in with a passkey. Plain HTTP is for a daemon on this device (no sign-in).")
                }
                if model.authPhase == .signedIn {
                    Section {
                        Button("Sign out", role: .destructive) {
                            Task {
                                await model.signOut()
                                dismiss()
                            }
                        }
                    } footer: {
                        Text("Revokes this device's access on the server and removes its tokens from the Keychain.")
                    }
                }
                if let error = model.lastError {
                    Section("Last error") { Text(error).foregroundStyle(.secondary) }
                }
            }
            .navigationTitle("Settings")
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done") {
                        Task {
                            await model.setServerURL(url)
                            dismiss()
                        }
                    }
                }
            }
            .onAppear { url = model.serverURL }
        }
    }
}
