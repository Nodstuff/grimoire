import GrimoireKit
import SwiftUI

@main
struct GrimoireApp: App {
    @State private var model = AppModel()
    @Environment(\.scenePhase) private var scenePhase

    var body: some Scene {
        WindowGroup {
            RootView()
                .environment(model)
                .task { await model.boot() }
        }
        .onChange(of: scenePhase) { _, phase in
            // foreground-only sync; APNs/background refresh come later
            Task {
                switch phase {
                case .active: await model.startSync()
                case .background: await model.stopSync()
                default: break
                }
            }
        }
    }
}
