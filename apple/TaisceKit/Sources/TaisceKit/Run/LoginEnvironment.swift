import Foundation

/// The person's login environment (PATH from their shell profile, GOPATH,
/// …), read once per launch with `$SHELL -l -c 'env -0'` and used for every
/// run. A GUI app's own environment is launchd's (PATH=/usr/bin:/bin:…),
/// which would miss Homebrew's `go`.
public enum LoginEnvironment {
    /// Variables never passed on from the capture: the shell's own
    /// bookkeeping, and the directory it happened to start in.
    static let dropped: Set<String> = ["_", "SHLVL", "PWD", "OLDPWD"]

    /// `env -0` output: NUL-separated `KEY=value` records (a value may hold
    /// newlines). Anything a login script printed before the first record
    /// is skipped; records that aren't `KEY=…` are dropped.
    public static func parse(_ data: Data) -> [String: String] {
        var env: [String: String] = [:]
        let records = data.split(separator: 0, omittingEmptySubsequences: true)
        for (i, rec) in records.enumerated() {
            var text = String(decoding: rec, as: UTF8.self)
            if i == 0, !isRecord(text), let nl = text.lastIndex(of: "\n") {
                // a profile's chatter ("Last login…") ahead of the first record
                text = String(text[text.index(after: nl)...])
            }
            guard isRecord(text), let eq = text.firstIndex(of: "=") else { continue }
            let key = String(text[..<eq])
            guard !dropped.contains(key) else { continue }
            env[key] = String(text[text.index(after: eq)...])
        }
        return env
    }

    static func isRecord(_ s: String) -> Bool {
        guard let eq = s.firstIndex(of: "="), eq != s.startIndex else { return false }
        let key = s[..<eq]
        guard let f = key.unicodeScalars.first, f == "_" || (f.isASCII && CharacterSet.letters.contains(f)) else { return false }
        return key.unicodeScalars.allSatisfy { $0 == "_" || ($0.isASCII && CharacterSet.alphanumerics.contains($0)) }
    }

    /// The environment the login shell starts from: just enough to find
    /// home and run a profile (not the app's own, which carries launchd's
    /// and the app's variables).
    public static func seed(home: String, user: String, shell: String, tmpdir: String?) -> [String: String] {
        var env = [
            "HOME": home, "USER": user, "LOGNAME": user, "SHELL": shell,
            "PATH": "/usr/bin:/bin:/usr/sbin:/sbin", "LANG": "en_US.UTF-8",
        ]
        if let tmpdir { env["TMPDIR"] = tmpdir }
        return env
    }

    /// The first directory on `path` holding an executable `name`.
    public static func which(_ name: String, path: String?, isExecutable: (String) -> Bool = { FileManager.default.isExecutableFile(atPath: $0) }) -> String? {
        for dir in (path ?? "").split(separator: ":") where !dir.isEmpty {
            let candidate = "\(dir)/\(name)"
            if isExecutable(candidate) { return candidate }
        }
        return nil
    }
}
