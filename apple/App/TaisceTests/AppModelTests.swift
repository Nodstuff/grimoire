import Foundation
import SwiftUI
import Testing
import TaisceKit
import UIKit
@testable import Taisce

/// First-launch states, in the real app process: the model boots, the views
/// lay out, and a failed sign-in leaves the app usable.
@MainActor @Suite(.serialized) struct AppModelTests {
    func model(server: String) async -> AppModel {
        UserDefaults.standard.set(server, forKey: AppModel.serverURLKey)
        let m = AppModel()
        await m.boot()
        await m.stopSync()
        return m
    }

    /// Lay out `view` in a real window, as the app would.
    func render(_ view: some View) throws {
        let scene = try #require(UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }.first)
        let window = UIWindow(windowScene: scene)
        let host = UIHostingController(rootView: view)
        window.rootViewController = host
        window.makeKeyAndVisible()
        host.view.layoutIfNeeded()
        window.isHidden = true
    }

    @Test func bootStartsUndecidedAndNormalisesTheURL() async throws {
        UserDefaults.standard.set("127.0.0.1:9", forKey: AppModel.serverURLKey)
        let m = AppModel()
        #expect(m.authPhase == .checking, "neither docs nor sign-in before boot decides")
        #expect(m.serverURL == "http://127.0.0.1:9")
        await m.boot()
        await m.stopSync()
        #expect(m.authPhase == .notRequired)
        await m.setServerURL("taisce.invalid")
        await m.stopSync()
        #expect(m.serverURL == "https://taisce.invalid")
        UserDefaults.standard.removeObject(forKey: AppModel.serverURLKey)
    }

    @Test func anUnreachableLocalDaemonShowsTheEmptyLibrary() async throws {
        // nothing listens on port 9: loopback is assumed LOCAL (no sign-in)
        let m = await model(server: "http://127.0.0.1:9")
        #expect(m.authPhase == .notRequired && !m.needsSignIn)
        #expect(m.cache != nil && m.api != nil)
        try render(RootView().environment(m))
        try render(NavigationStack { TodayScreen() }.environment(m).environment(Router()))
        try render(NavigationStack { DocScreen(docID: "missing") }.environment(m).environment(Router()))
        try render(SettingsScreen().environment(m))
    }

    @Test func aSingleDocLibraryRenders() async throws {
        let m = await model(server: "http://127.0.0.1:9")
        let cache = try #require(m.cache)
        let id = "it-\(UUID().uuidString.prefix(8))"
        try await cache.storeDoc(DocTree(
            doc: DocSummary(id: id, parentID: nil, title: "Only", currentEpoch: 1),
            roots: [BlockNode(block: Block(id: "\(id)-b", docID: id, parentID: nil, orderKey: "i", blockType: .paragraph, content: "hello [[Only]]"))]
        ))
        // the sidebar's tree comes from a GRDB observation: give it a moment
        for _ in 0..<50 where !m.docs.contains(where: { $0.id == id }) {
            try await Task.sleep(for: .milliseconds(40))
        }
        for _ in 0..<50 where !m.library.contains(where: { $0.id == id }) {
            try await Task.sleep(for: .milliseconds(40))
        }
        #expect(m.library.contains { $0.id == id })
        #expect(m.index.doc(titled: "Only")?.id == id)
        try render(RootView().environment(m))
        try render(NavigationStack { DocScreen(docID: id) }.environment(m).environment(Router()))
        try await cache.deleteDoc(id)
    }

    @Test func aFailedSignInLeavesTheSignInScreenUsable() async throws {
        // unreachable and not loopback: asks to sign in
        let m = await model(server: "https://taisce.invalid")
        #expect(m.authPhase == .signedOut && m.needsSignIn)
        try render(RootView().environment(m))
        await m.signIn()
        #expect(!m.isSigningIn, "the button is enabled again")
        #expect(m.lastError?.hasPrefix("Sign-in failed") == true)
        #expect(m.needsSignIn)
        // switching to a working (local) server recovers
        await m.setServerURL("http://127.0.0.1:9")
        await m.stopSync()
        #expect(!m.needsSignIn)
        try render(SignInScreen().environment(m))
        UserDefaults.standard.removeObject(forKey: AppModel.serverURLKey)
    }
}
