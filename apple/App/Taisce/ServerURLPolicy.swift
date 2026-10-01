import Foundation

/// Which server addresses the app accepts: HTTPS only (no plaintext, end to
/// end). Debug builds alone also take plain HTTP to a loopback daemon, the
/// one ATS exception the Debug Info.plist carries.
enum ServerURLPolicy {
    enum Rejection: Error, Equatable {
        case notAURL
        case insecure

        var message: String {
            switch self {
            case .notAURL: "Not a server address. Use https://host[:port]."
            case .insecure: "The server address must start with https://."
            }
        }
    }

    #if DEBUG
    static let allowsLoopbackHTTP = true
    #else
    static let allowsLoopbackHTTP = false
    #endif

    static func check(_ s: String, allowLoopbackHTTP: Bool = allowsLoopbackHTTP) -> Result<URL, Rejection> {
        guard let url = URL(string: s), let scheme = url.scheme?.lowercased(),
              let host = url.host(), !host.isEmpty
        else { return .failure(.notAURL) }
        switch scheme {
        case "https":
            return .success(url)
        case "http":
            let loopback = ["127.0.0.1", "localhost", "::1"].contains(host)
            return allowLoopbackHTTP && loopback ? .success(url) : .failure(.insecure)
        default:
            return .failure(.notAURL)
        }
    }

    static func accepts(_ s: String) -> Bool {
        (try? check(s).get()) != nil
    }
}
