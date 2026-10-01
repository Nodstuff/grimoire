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
                    Text("Plain HTTP works for this device and the local network only; use HTTPS for anything else.")
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
