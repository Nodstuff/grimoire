import Foundation

/// Whose data the cache holds (ADR 0004: one server, several people). The
/// cache is keyed to the signed-in user's human principal (`/api/profile`
/// `principal_id`), so a different person signing in on this device starts
/// from an empty cache, never from someone else's docs or queued writes.
public enum CacheOwnership {
    public enum Decision: Sendable, Hashable {
        /// the cache is theirs already
        case keep
        /// first time we learn whose it is, and nothing says it's someone else's
        case adopt(String)
        /// someone else's (or unknown) data: wipe, then record `owner` if known
        case wipe(owner: String?)
    }

    /// - `cached`: the owner recorded in the cache (nil: never recorded,
    ///   e.g. a build before this one).
    /// - `current`: the token's user, nil when the profile couldn't be read.
    /// - `freshSignIn`: a sign-in just happened in this process (as opposed
    ///   to resuming with tokens already in the Keychain).
    /// - `cacheEmpty`: nothing cached at all.
    public static func decide(cached: String?, current: String?, freshSignIn: Bool, cacheEmpty: Bool) -> Decision {
        switch (cached, current) {
        case let (c?, u?):
            return c == u ? .keep : .wipe(owner: u)
        case let (nil, u?):
            // a fresh sign-in over data nobody claimed (left by an older
            // build, or a sign-out we didn't see) may be someone else's
            return freshSignIn && !cacheEmpty ? .wipe(owner: u) : .adopt(u)
        case (_, nil):
            // can't tell who is signed in: a resume keeps what it has (the
            // tokens are the same person's); a fresh sign-in over unclaimed
            // or claimed data doesn't risk showing it to someone else
            return freshSignIn && !cacheEmpty ? .wipe(owner: nil) : .keep
        }
    }

    /// Read the profile, decide, and wipe or record the owner. Returns what
    /// was done, so the app can clear its own per-user state on a wipe.
    @discardableResult
    public static func reconcile(cache: Cache, api: APIClient, freshSignIn: Bool) async throws -> Decision {
        let current = try? await api.profile().principalID
        let decision = decide(
            cached: try await cache.owner(), current: current,
            freshSignIn: freshSignIn, cacheEmpty: try await cache.isEmpty()
        )
        switch decision {
        case .keep: break
        case let .adopt(owner): try await cache.setOwner(owner)
        case let .wipe(owner):
            try await cache.wipe()
            if let owner { try await cache.setOwner(owner) }
        }
        return decision
    }
}
