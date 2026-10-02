import TaisceKit
import SwiftUI

@main
struct TaisceApp: App {
    // push: the delegate owns the model, so a background launch has one
    @UIApplicationDelegateAdaptor(AppDelegate.self) private var appDelegate
    private var model: AppModel { appDelegate.model }
    @Environment(\.scenePhase) private var scenePhase

    var body: some Scene {
        WindowGroup {
            RootView()
                .environment(model)
                // a test host (hosted unit tests) never connects: tests make their own models
                .task { if !AppPaths.isTestHost { await model.boot() } }
        }
        .commands { TaisceCommands(model: model) }
        .onChange(of: scenePhase) { _, phase in
            // the stream is foreground-only; silent pushes catch up in the background (AppDelegate)
            Task {
                guard !AppPaths.isTestHost else { return }
                switch phase {
                case .active: await model.startSync()
                case .background: await model.stopSync()
                default: break
                }
            }
        }
    }
}
