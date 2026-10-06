import Darwin
import Foundation

/// Why a linked image wasn't fetched.
public enum ShareImageRejection: Error, Sendable, Hashable, LocalizedError {
    case notHTTPS
    case ipLiteral
    case localName
    case privateAddress(String)
    case unresolvable
    case tooManyRedirects
    case tooLarge
    case notAnImage
    case http(Int)
    case notApproved

    public var errorDescription: String? {
        switch self {
        case .notHTTPS: "only https images are fetched"
        case .ipLiteral: "an address instead of a name"
        case .localName: "a local or internal name"
        case let .privateAddress(a): "it points at a private address (\(a))"
        case .unresolvable: "its name doesn't resolve"
        case .tooManyRedirects: "too many redirects"
        case .tooLarge: "larger than 2 MB"
        case .notAnImage: "not a PNG, JPEG, WebP or SVG image"
        case let .http(s): "the server answered \(s)"
        case .notApproved: "its site wasn't one you agreed to fetch from"
        }
    }
}

/// An address a name resolved to, in its family.
public enum ResolvedAddress: Sendable, Hashable, CustomStringConvertible {
    case v4([UInt8])
    case v6([UInt8])

    public var description: String {
        switch self {
        case let .v4(b): return b.map(String.init).joined(separator: ".")
        case let .v6(b):
            var a = in6_addr()
            withUnsafeMutableBytes(of: &a) { $0.copyBytes(from: b) }
            var buf = [CChar](repeating: 0, count: Int(INET6_ADDRSTRLEN))
            inet_ntop(AF_INET6, &a, &buf, socklen_t(buf.count))
            return String(cString: buf)
        }
    }

    /// Reachable on the public internet: not loopback, private (RFC 1918),
    /// CGNAT, link-local, unique-local, multicast, unspecified or reserved.
    public var isPublic: Bool {
        switch self {
        case let .v4(b):
            guard b.count == 4 else { return false }
            switch (b[0], b[1]) {
            case (0, _), (10, _), (127, _): return false
            case (100, 64...127): return false // CGNAT 100.64/10
            case (169, 254): return false // link-local
            case (172, 16...31): return false
            case (192, 168): return false
            case (192, 0) where b[2] == 0 || b[2] == 2: return false // IETF, TEST-NET-1
            case (198, 18...19): return false // benchmarking
            case (198, 51) where b[2] == 100, (203, 0) where b[2] == 113: return false // TEST-NETs
            case (224...255, _): return false // multicast, reserved, broadcast
            default: return true
            }
        case let .v6(b):
            guard b.count == 16 else { return false }
            if b.allSatisfy({ $0 == 0 }) { return false } // ::
            if b[0..<15].allSatisfy({ $0 == 0 }) && b[15] == 1 { return false } // ::1
            if b[0] == 0xFF { return false } // multicast ff00::/8
            if b[0] == 0xFE && (b[1] & 0xC0) == 0x80 { return false } // link-local fe80::/10
            if b[0] == 0xFE && (b[1] & 0xC0) == 0xC0 { return false } // site-local fec0::/10
            if (b[0] & 0xFE) == 0xFC { return false } // ULA fc00::/7
            // IPv4-mapped ::ffff:a.b.c.d and IPv4-compatible ::a.b.c.d
            if b[0..<10].allSatisfy({ $0 == 0 }) && ((b[10] == 0xFF && b[11] == 0xFF) || (b[10] == 0 && b[11] == 0)) {
                return ResolvedAddress.v4(Array(b[12..<16])).isPublic
            }
            // NAT64 64:ff9b::/96
            if b[0..<12] == [0x00, 0x64, 0xFF, 0x9B, 0, 0, 0, 0, 0, 0, 0, 0] {
                return ResolvedAddress.v4(Array(b[12..<16])).isPublic
            }
            // 6to4 2002::/16 carries an IPv4 address
            if b[0] == 0x20 && b[1] == 0x02 { return ResolvedAddress.v4(Array(b[2..<6])).isPublic }
            if b[0] == 0x20 && b[1] == 0x01 && b[2] == 0x0D && b[3] == 0xB8 { return false } // documentation
            return true
        }
    }
}

/// Name → addresses. The real one is getaddrinfo; tests fake it.
public protocol HostResolving: Sendable {
    func resolve(_ host: String) async throws -> [ResolvedAddress]
}

public struct SystemHostResolver: HostResolving {
    public init() {}

    public func resolve(_ host: String) async throws -> [ResolvedAddress] {
        try await Task.detached {
            var hints = addrinfo()
            hints.ai_family = AF_UNSPEC
            hints.ai_socktype = SOCK_STREAM
            var res: UnsafeMutablePointer<addrinfo>?
            guard getaddrinfo(host, nil, &hints, &res) == 0, let first = res else { throw ShareImageRejection.unresolvable }
            defer { freeaddrinfo(first) }
            var out: [ResolvedAddress] = []
            var p: UnsafeMutablePointer<addrinfo>? = first
            while let ai = p {
                if ai.pointee.ai_family == AF_INET, let sa = ai.pointee.ai_addr {
                    sa.withMemoryRebound(to: sockaddr_in.self, capacity: 1) { s in
                        out.append(.v4(withUnsafeBytes(of: s.pointee.sin_addr) { Array($0) }))
                    }
                } else if ai.pointee.ai_family == AF_INET6, let sa = ai.pointee.ai_addr {
                    sa.withMemoryRebound(to: sockaddr_in6.self, capacity: 1) { s in
                        out.append(.v6(withUnsafeBytes(of: s.pointee.sin6_addr) { Array($0) }))
                    }
                }
                p = ai.pointee.ai_next
            }
            return out
        }.value
    }
}

/// Which linked images a snapshot may fetch, by URL and by where the name
/// points. A DNS rebind between this check and the fetch is still possible
/// (the system resolves again); accepted, as the fetch is a plain GET whose
/// answer must sniff as an image and is published only to the owner's link.
public struct ShareImagePolicy: Sendable {
    public var resolver: any HostResolving

    public init(resolver: any HostResolving = SystemHostResolver()) {
        self.resolver = resolver
    }

    /// The name alone: https, a real multi-label public name, no address literal.
    public static func check(_ url: URL) -> ShareImageRejection? {
        guard url.scheme?.lowercased() == "https" else { return .notHTTPS }
        guard url.user == nil, url.password == nil else { return .localName }
        guard var host = url.host(percentEncoded: false)?.lowercased(), !host.isEmpty else { return .localName }
        while host.hasSuffix(".") { host.removeLast() }
        if host.hasPrefix("[") || host.contains(":") { return .ipLiteral }
        if isIPLiteral(host) { return .ipLiteral }
        let labels = host.split(separator: ".", omittingEmptySubsequences: false)
        if labels.count < 2 || labels.contains(where: \.isEmpty) { return .localName }
        if host == "localhost" || ["localhost", "local", "internal", "lan", "home", "corp", "intranet", "localdomain", "home.arpa"].contains(where: { host.hasSuffix("." + $0) }) {
            return .localName
        }
        return nil
    }

    /// Dotted, decimal, octal or hex IPv4 in any of inet_aton's forms, or IPv6.
    static func isIPLiteral(_ host: String) -> Bool {
        var a4 = in_addr(), a6 = in6_addr()
        if inet_pton(AF_INET, host, &a4) == 1 || inet_pton(AF_INET6, host, &a6) == 1 { return true }
        if inet_aton(host, &a4) != 0 { return true } // 2130706433, 0x7f.1, 0177.0.0.1
        // a last label of digits or 0x… is never a public name
        let last = host.split(separator: ".").last.map(String.init) ?? host
        return last.allSatisfy(\.isNumber) || last.hasPrefix("0x")
    }

    /// The name, then every address it resolves to.
    public func allows(_ url: URL) async -> ShareImageRejection? {
        if let r = Self.check(url) { return r }
        guard let host = url.host(percentEncoded: false) else { return .localName }
        guard let addresses = try? await resolver.resolve(host), !addresses.isEmpty else { return .unresolvable }
        if let bad = addresses.first(where: { !$0.isPublic }) { return .privateAddress(bad.description) }
        return nil
    }
}

/// Fetches linked images for a snapshot, carefully: https only, public
/// addresses only (checked again on every redirect, at most 3), an ephemeral
/// session with no cookies, cache or credentials, every auth challenge
/// refused, 2 MB at most (streamed, stopped past it), and the type taken
/// from the bytes, never the server's Content-Type.
public final class SafeShareImageLoader: NSObject, ShareImageLoading, URLSessionTaskDelegate, @unchecked Sendable {
    public static let maxRedirects = 3
    let policy: ShareImagePolicy
    let configuration: URLSessionConfiguration
    let maxBytes: Int
    /// The sites the person agreed to fetch from (the first URL's host must
    /// be one; redirects are checked by the policy). nil: any public site.
    public var allowedHosts: Set<String>?

    /// `configuration`: tests pass one routed to a mock; it is made ephemeral
    /// regardless (no cookies, cache or credential storage).
    public init(policy: ShareImagePolicy = ShareImagePolicy(), configuration: URLSessionConfiguration = .ephemeral, maxBytes: Int = ShareLimits.assetBytes) {
        self.policy = policy
        let c = configuration
        c.httpCookieStorage = nil
        c.httpShouldSetCookies = false
        c.httpCookieAcceptPolicy = .never
        c.urlCache = nil
        c.requestCachePolicy = .reloadIgnoringLocalCacheData
        c.urlCredentialStorage = nil
        // a system proxy or PAC file would resolve and connect for itself,
        // past the address check: never use one
        c.connectionProxyDictionary = [:]
        c.timeoutIntervalForRequest = 10
        c.timeoutIntervalForResource = 20
        self.configuration = c
        self.maxBytes = maxBytes
    }

    public func load(_ url: URL) async throws -> RenderedVisual {
        if let allowedHosts, !allowedHosts.contains(url.host()?.lowercased() ?? "") { throw ShareImageRejection.notApproved }
        if let r = await policy.allows(url) { throw r }
        let session = URLSession(configuration: configuration, delegate: self, delegateQueue: nil)
        defer {
            session.invalidateAndCancel()
            forget(session)
        }
        var request = URLRequest(url: url)
        request.setValue("image/png, image/jpeg, image/webp, image/svg+xml", forHTTPHeaderField: "Accept")
        let (bytes, response) = try await session.bytes(for: request)
        guard let http = response as? HTTPURLResponse else { throw ShareImageRejection.notAnImage }
        guard (200..<300).contains(http.statusCode) else { throw ShareImageRejection.http(http.statusCode) }
        if http.expectedContentLength > Int64(maxBytes) { throw ShareImageRejection.tooLarge }
        var data = Data()
        data.reserveCapacity(min(Int(max(http.expectedContentLength, 0)), maxBytes))
        for try await chunk in bytes.chunks(64 * 1024) {
            data.append(contentsOf: chunk)
            if data.count > maxBytes { throw ShareImageRejection.tooLarge }
        }
        guard !data.isEmpty, let type = Self.sniff(data) else { throw ShareImageRejection.notAnImage }
        let size = type == "image/png" ? Self.pngSize(data) : nil
        return RenderedVisual(data: data, contentType: type, width: size?.0, height: size?.1)
    }

    // MARK: URLSessionTaskDelegate

    /// Every hop is checked like the first, and there are at most 3.
    public func urlSession(_ session: URLSession, task: URLSessionTask, willPerformHTTPRedirection response: HTTPURLResponse, newRequest request: URLRequest) async -> URLRequest? {
        await allowRedirect(to: request.url, hops: redirectCount(session, task) + 1) ? request : nil
    }

    func allowRedirect(to url: URL?, hops: Int) async -> Bool {
        guard hops <= Self.maxRedirects, let url else { return false }
        return await policy.allows(url) == nil
    }

    private let hops = NSLock()
    /// Hops so far per (session, task): each load has its own session, whose
    /// task identifiers start again at 1, so the session is part of the key.
    private var counts: [HopKey: Int] = [:]
    private struct HopKey: Hashable { let session: ObjectIdentifier; let task: Int }

    private func redirectCount(_ session: URLSession, _ task: URLSessionTask) -> Int {
        hops.withLock {
            let k = HopKey(session: ObjectIdentifier(session), task: task.taskIdentifier)
            let n = counts[k, default: 0]
            counts[k] = n + 1
            return n
        }
    }

    private func forget(_ session: URLSession) {
        let id = ObjectIdentifier(session)
        hops.withLock { counts = counts.filter { $0.key.session != id } }
    }

    /// No credentials, ever (server trust still gets the system's checks).
    public func urlSession(_ session: URLSession, task: URLSessionTask, didReceive challenge: URLAuthenticationChallenge) async -> (URLSession.AuthChallengeDisposition, URLCredential?) {
        challenge.protectionSpace.authenticationMethod == NSURLAuthenticationMethodServerTrust
            ? (.performDefaultHandling, nil) : (.cancelAuthenticationChallenge, nil)
    }

    // MARK: sniffing

    static func sniff(_ d: Data) -> String? {
        let b = [UInt8](d.prefix(16))
        if b.starts(with: [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]) { return "image/png" }
        if b.starts(with: [0xFF, 0xD8, 0xFF]) { return "image/jpeg" }
        if b.count >= 12, b.starts(with: Array("RIFF".utf8)), Array(b[8..<12]) == Array("WEBP".utf8) { return "image/webp" }
        let head = String(decoding: d.prefix(1024), as: UTF8.self).trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        if head.hasPrefix("<svg") || ((head.hasPrefix("<?xml") || head.hasPrefix("<!doctype svg")) && head.contains("<svg")) { return "image/svg+xml" }
        return nil
    }

    /// Width and height from a PNG's IHDR.
    static func pngSize(_ d: Data) -> (Int, Int)? {
        let b = [UInt8](d.prefix(24))
        guard b.count == 24 else { return nil }
        func be(_ i: Int) -> Int { Int(b[i]) << 24 | Int(b[i + 1]) << 16 | Int(b[i + 2]) << 8 | Int(b[i + 3]) }
        return (be(16), be(20))
    }
}

extension URLSession.AsyncBytes {
    /// The bytes in chunks of up to `size` (byte-at-a-time iteration, batched).
    func chunks(_ size: Int) -> AsyncThrowingStream<[UInt8], any Error> {
        AsyncThrowingStream { continuation in
            let task = Task {
                var buf: [UInt8] = []
                buf.reserveCapacity(size)
                do {
                    for try await byte in self {
                        buf.append(byte)
                        if buf.count >= size {
                            continuation.yield(buf)
                            buf.removeAll(keepingCapacity: true)
                        }
                    }
                    if !buf.isEmpty { continuation.yield(buf) }
                    continuation.finish()
                } catch {
                    continuation.finish(throwing: error)
                }
            }
            continuation.onTermination = { _ in task.cancel() }
        }
    }
}
