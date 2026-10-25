import Foundation
import Observation
import SwiftUI
import TaisceKit

/// One open book (or doc read as a book): the page, where you are, the
/// settings, and the sync with every other device (`ui/src/reader/sync.ts`
/// rules via `ReaderSync` / `PositionWriter`).
@MainActor @Observable
final class ReaderModel {
    enum Phase: Equatable {
        case loading
        case reading
        case failed(String)
    }

    let source: ReadingSource
    var title: String
    @ObservationIgnored let page = ReaderPage()
    @ObservationIgnored private weak var store: BookStore?

    private(set) var phase: Phase = .loading
    private(set) var place: ReaderEvent.Place?
    private(set) var toc: [ReaderEvent.TOCItem] = []
    var chromeVisible = true
    var toast: ToastMessage?
    var settings: ReaderSettings {
        didSet {
            ReaderSettingsStore.save(settings)
            applyLook()
        }
    }

    /// the server keeps positions for us (SERVER mode)
    private(set) var syncs = false
    @ObservationIgnored private var writer: PositionWriter?
    /// a page was turned since the book opened (or since the last jump):
    /// only the reader's own moves are written
    @ObservationIgnored private var armed = false
    @ObservationIgnored private var dark = false
    @ObservationIgnored private var phone = true
    @ObservationIgnored private var reduceMotion = false
    @ObservationIgnored private var started = false
    @ObservationIgnored private var seenRemote: Int64 = 0

    var deviceID: String { ReaderPlaces.deviceID }

    /// The store arrives with the environment (the model is made in the view's `init`).
    func attach(_ store: BookStore) {
        if self.store == nil { self.store = store }
    }

    init(source: ReadingSource, title: String, store: BookStore?) {
        self.source = source
        self.title = title
        self.store = store
        settings = ReaderSettingsStore.load()
        page.onEvent = { [weak self] e in self?.handle(e) }
    }

    var fraction: Double { place?.fraction ?? 0 }
    var percentText: String { "\(ReaderFormat.percent(place?.fraction))%" }
    var chapter: String { place?.chapter ?? "" }
    var minutesLeft: String? { ReaderFormat.minutesLeft(place?.minutes) }
    var paper: ReaderSettings.Paper { settings.resolvedPaper(dark: dark) }

    func environment(dark: Bool, phone: Bool, reduceMotion: Bool) {
        let changed = dark != self.dark || phone != self.phone || reduceMotion != self.reduceMotion
        self.dark = dark
        self.phone = phone
        self.reduceMotion = reduceMotion
        if changed, started { applyLook() }
    }

    private var look: ReaderLook { settings.look(dark: dark, phone: phone, reduceMotion: reduceMotion) }

    private func applyLook() { page.setLook(look) }

    // MARK: open

    /// Open the book (once: a second call waits for the first). The work
    /// is the model's own task, not the caller's: SwiftUI can cancel a view's
    /// `.task` and run it again (a cover presented while a sheet was still
    /// going away did exactly that), and a cancelled download read as
    /// "offline" for good. A start that ends cancelled anyway is forgotten,
    /// so the next `start()` downloads again.
    func start() async {
        if opening == nil {
            opening = Task { [weak self] in await self?.open() }
        }
        await opening?.value
    }

    /// The failed card's Try Again.
    func retry() {
        guard canRetry else { return }
        opening = nil
        Task { await start() }
    }

    /// How the file and the server's place arrive: the store's API, or a test's.
    struct Loader: Sendable {
        var file: @MainActor @Sendable (ReadingSource) async throws -> URL
        var position: @Sendable (ReadingSource) async -> PositionRead
    }

    /// set by tests; nil = the store's
    @ObservationIgnored var loader: Loader?
    @ObservationIgnored private var opening: Task<Void, Never>?
    /// the failed card offers Try Again
    private(set) var canRetry = false

    private var storeLoader: Loader? {
        guard let store, let api = store.api else { return nil }
        return Loader(file: { [weak store] source in
            guard let store else { throw CancellationError() }
            return try await store.epubFile(source)
        }, position: { source in
            (try? await api.position(source)) ?? .unavailable
        })
    }

    private func open() async {
        canRetry = false
        guard let loader = loader ?? storeLoader else {
            phase = .failed("Sign in to read.")
            return
        }
        phase = .loading
        let source = self.source
        async let fileTask = loader.file(source)
        async let positionTask = loader.position(source)
        let remoteRead = await positionTask
        let file: URL
        do {
            file = try await fileTask
        } catch {
            if Self.isCancellation(error) { opening = nil }
            canRetry = Self.isRetryable(error)
            phase = .failed(Self.describe(error))
            return
        }
        started = true
        let server = store?.serverKey ?? ""
        let local = ReaderPlaces.load(source, server: server)
        var remote: ReaderPosition?
        if case let .available(r) = remoteRead {
            syncs = true
            remote = r
            seenRemote = max(r?.timestamp ?? 0, local?.timestamp ?? 0)
            makeWriter(seen: seenRemote)
        }
        let target = ReaderSync.startTarget(remote: remote, local: local, mine: deviceID)
        if case let .xpointer(_, _, from) = target, let note = ReaderSync.syncToast(from, mine: deviceID) {
            pendingToast = note
        }
        page.open(book: file, start: target, look: look)
    }

    @ObservationIgnored private var pendingToast: String?

    private func makeWriter(seen: Int64) {
        guard let api = store?.api else { return }
        let source = self.source
        writer = PositionWriter(
            device: deviceID, name: ReaderPlaces.deviceName, seen: seen,
            put: { body in await api.putPosition(source, body) },
            onNewer: { [weak self] p in await self?.takeNewer(p) },
            onWritten: { [weak self] p in await self?.written(p) }
        )
    }

    // MARK: page events

    private func handle(_ e: ReaderEvent) {
        switch e {
        case .loaded: break
        case let .opened(title, toc):
            self.toc = toc
            if self.title.isEmpty, !title.isEmpty { self.title = title }
        case .ready:
            phase = .reading
            if let note = pendingToast {
                pendingToast = nil
                toast = ToastMessage(text: note, symbol: "arrow.triangle.2.circlepath")
            }
            // the chrome steps aside once the page is up (VoiceOver keeps it)
            if !UIAccessibility.isVoiceOverRunning {
                Task {
                    try? await Task.sleep(for: .seconds(1.5))
                    if !self.armed { withAnimation(Lapis.Motion.standard) { self.chromeVisible = false } }
                }
            }
        case let .relocate(p):
            place = p
            remember(p)
            if armed, let writer {
                let here = ReaderHere(progress: p.xpointer, percentage: p.fraction)
                Task { await writer.moved(here) }
            }
        case .moved:
            armed = true
            if chromeVisible, !UIAccessibility.isVoiceOverRunning {
                withAnimation(Lapis.Motion.fast) { chromeVisible = false }
            }
        case .toggleChrome:
            withAnimation(Lapis.Motion.standard) { chromeVisible.toggle() }
        case let .failed(message):
            phase = .failed(message)
        }
    }

    /// This device's place (finer than an XPointer) and the shelf's progress.
    private func remember(_ p: ReaderEvent.Place) {
        guard let store else { return }
        let stamp = max(seenRemote, 0)
        if !p.cfi.isEmpty {
            ReaderPlaces.save(LocalPlace(cfi: p.cfi, fraction: p.fraction, timestamp: stamp), source, server: store.serverKey)
        }
        if case let .book(id) = source, armed {
            store.noteProgress(id, fraction: p.fraction, device: ReaderPlaces.deviceName)
        }
    }

    private func written(_ p: ReaderPosition) {
        seenRemote = max(seenRemote, p.timestamp)
        if let place, !place.cfi.isEmpty, let store {
            ReaderPlaces.save(LocalPlace(cfi: place.cfi, fraction: place.fraction, timestamp: p.timestamp), source, server: store.serverKey)
        }
    }

    /// Another device moved on: go there (not drag it back) and say so.
    private func takeNewer(_ p: ReaderPosition) async {
        await writer?.discard()
        await writer?.seen(p.timestamp)
        seenRemote = max(seenRemote, p.timestamp)
        armed = false
        page.goTo(xpointer: p.progress, fraction: p.percentage)
        if let note = ReaderSync.syncToast(p, mine: deviceID) {
            toast = ToastMessage(text: note, symbol: "arrow.triangle.2.circlepath")
        }
    }

    // MARK: life

    /// Back in front: has another device moved on meanwhile?
    func resumed() async {
        guard syncs, phase == .reading, let api = store?.api else { return }
        guard case let .available(remote)? = try? await api.position(source) else { return }
        if ReaderSync.isNewer(remote, seen: seenRemote, mine: deviceID), let remote {
            await takeNewer(remote)
        } else if let remote {
            seenRemote = max(seenRemote, remote.timestamp)
            await writer?.seen(remote.timestamp)
        }
    }

    /// Going to the background: write the place now.
    func backgrounded() async {
        await writer?.flush()
    }

    func close() async {
        await writer?.flush()
        await writer?.stop()
        writer = nil
    }

    // MARK: commands

    func next() { armed = true; page.next() }
    func prev() { armed = true; page.prev() }

    func go(_ item: ReaderEvent.TOCItem) {
        armed = true
        page.goTo(href: item.href)
    }

    func go(fraction: Double) {
        armed = true
        page.goTo(fraction: fraction)
    }

    /// Only a connection that isn't there says "offline"; anything else
    /// says what happened.
    static let offlineCodes: Set<URLError.Code> = [
        .notConnectedToInternet, .networkConnectionLost, .timedOut, .cannotFindHost, .cannotConnectToHost, .dataNotAllowed,
    ]

    static func isCancellation(_ error: any Error) -> Bool {
        error is CancellationError || (error as? URLError)?.code == .cancelled
    }

    /// Worth a Try Again (a book gone from the server isn't).
    static func isRetryable(_ error: any Error) -> Bool {
        if case APIError.notFound = error { return false }
        return true
    }

    static func describe(_ error: any Error) -> String {
        if case APIError.notFound = error { return "This book isn't on the server any more." }
        if isCancellation(error) { return "Opening the book was interrupted." }
        if let e = error as? URLError {
            if offlineCodes.contains(e.code) { return "You're offline, and this book isn't on this device yet." }
            return "The book didn't download (\(e.localizedDescription))."
        }
        if error is CocoaError { return "The book couldn't be saved on this device." }
        return BookStore.describe(error)
    }
}

/// The reader's settings on this device (a preference: survives sign-out).
enum ReaderSettingsStore {
    static let key = "reader.settings"

    static func load() -> ReaderSettings {
        guard let data = UserDefaults.standard.data(forKey: key),
              let s = try? JSONDecoder().decode(ReaderSettings.self, from: data)
        else { return .defaults }
        return s
    }

    static func save(_ s: ReaderSettings) {
        UserDefaults.standard.set(try? JSONEncoder().encode(s), forKey: key)
    }
}
