import Foundation

/// Fractional sibling order keys, a port of crates/store/src/order_key.rs:
/// a base-36 fraction in (0, 1) written as digits, never ending in '0'.
/// Siblings sort by plain string comparison of their keys, so a key minted
/// here lands exactly where the server's own `between` would put it.
public enum OrderKey {
    static let digits = Array("0123456789abcdefghijklmnopqrstuvwxyz".utf8)
    static let base = 36

    /// Non-empty, lowercase base36 only, no trailing '0' (nothing fits
    /// between `x` and `x0`).
    public static func isValid(_ key: String) -> Bool {
        !key.isEmpty && key.utf8.allSatisfy { digits.contains($0) } && !key.hasSuffix("0")
    }

    /// A key strictly between `a` and `b`; nil is the open end (start or end
    /// of the sibling list). Total, with the server's fallback: when the
    /// bounds are invalid or unordered, the key lands just after the highest
    /// valid one.
    public static func between(_ a: String?, _ b: String?) -> String {
        let inputsValid = (a.map(isValid) ?? true) && (b.map(isValid) ?? true)
        if inputsValid {
            guard let a, let b, a >= b else { return bisect(a ?? "", b) }
        }
        let bounds = [a, b].compactMap { $0 }.filter(isValid)
        return bisect(bounds.max() ?? "", nil)
    }

    private static func value(_ c: UInt8) -> Int {
        digits.firstIndex(of: c) ?? 0
    }

    /// Precondition: both valid, and `a < b` when `b` is given.
    private static func bisect(_ a: String, _ b: String?) -> String {
        let ab = Array(a.utf8)
        let bb = b.map { Array($0.utf8) }
        func da(_ i: Int) -> Int { i < ab.count ? value(ab[i]) : 0 }
        var out: [UInt8] = []
        var i = 0
        while true {
            let lo = da(i)
            let hi = bb.map { i < $0.count ? value($0[i]) : 0 } ?? base
            if lo == hi {
                out.append(digits[lo])
                i += 1
                continue
            }
            if hi - lo > 1 {
                out.append(digits[(lo + hi) / 2])
                return String(decoding: out, as: UTF8.self)
            }
            // hi == lo + 1: keep lo, then bisect between the rest of a and 1
            out.append(digits[lo])
            i += 1
            while true {
                let lo = da(i)
                if base - lo > 1 {
                    out.append(digits[(lo + base) / 2])
                    return String(decoding: out, as: UTF8.self)
                }
                out.append(digits[lo])
                i += 1
            }
        }
    }
}
