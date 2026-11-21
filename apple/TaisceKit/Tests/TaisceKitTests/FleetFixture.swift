import Foundation

/// The fleet's JSON as a real daemon sent it: recorded on 2026-11-20 from a
/// scratch SERVER-mode daemon of this branch, with the fake runtime
/// (`crates/runtime-proto/examples/fake_runtime.rs`) playing its demo script,
/// one session snoozed and lifted, another snoozed, and the runtime then
/// killed. Nothing here is typed by hand except where a comment says so.
enum FleetFixture {
    static let runtime = "01a1fb15-5eea-78d0-a6b7-adbb1135747c"
    static let taisce = "0196f3a0-0000-7000-8000-000000000001"
    static let workbox = "0196f3a0-0000-7000-8000-000000000002"
    static let qompass = "0196f3a0-0000-7000-8000-000000000003"

    /// `GET /api/fabric/sessions`: a runtime enrolled and never connected.
    static let neverConnected = #"""
{"rev":1,"runtimes":[{"id":"01a1fb15-5eea-78d0-a6b7-adbb1135747c","name":"mbp","created_at":1795213844,"online":false,"last_seen_at":null,"version":"","accounts":[],"accounts_at":null}],"sessions":[]}
"""#

    /// `GET /api/fabric/sessions`: the runtime online, three sessions in the server's order.
    static let live = #"""
{"rev":9,"runtimes":[{"id":"01a1fb15-5eea-78d0-a6b7-adbb1135747c","name":"mbp","created_at":1795213844,"online":true,"last_seen_at":1795213871,"version":"fake/0.8.0","accounts":[{"name":"personal","session_resets_at":null,"session_used_pct":38,"week_resets_at":null,"week_used_pct":61},{"name":"work","session_resets_at":null,"session_used_pct":92,"week_resets_at":null,"week_used_pct":70}],"accounts_at":1795213858}],"sessions":[{"id":"0196f3a0-0000-7000-8000-000000000001","runtime_id":"01a1fb15-5eea-78d0-a6b7-adbb1135747c","address":"taisce/main","fabric":"runtime-channel","project":"taisce","worktree":"main","branch":"agent-fabric","account":"personal","state":"needs_you","tool":"","note":"","last_line":"The tests pass. Shall I open the pull request?","context_used_pct":44,"since":1795213864,"updated_at":1795213864,"quiet_until":null},{"id":"0196f3a0-0000-7000-8000-000000000002","runtime_id":"01a1fb15-5eea-78d0-a6b7-adbb1135747c","address":"workbox/main","fabric":"runtime-channel","project":"workbox","worktree":"main","branch":"main","account":"work","state":"finished","tool":"","note":"","last_line":"Done: the hook receiver checks Host, Origin and the token.","context_used_pct":19,"since":1795213869,"updated_at":1795213869,"quiet_until":null},{"id":"0196f3a0-0000-7000-8000-000000000003","runtime_id":"01a1fb15-5eea-78d0-a6b7-adbb1135747c","address":"qompass/mcp-dx","fabric":"qompass","project":"qompass","worktree":"mcp-dx","branch":"mcp-dx","account":"work","state":"asleep","tool":"","note":"","last_line":"","context_used_pct":null,"since":1795213858,"updated_at":1795213858,"quiet_until":null}]}
"""#

    /// `GET /api/fabric/sessions` after the runtime's stream ended; qompass/mcp-dx is snoozed.
    static let offline = #"""
{"rev":13,"runtimes":[{"id":"01a1fb15-5eea-78d0-a6b7-adbb1135747c","name":"mbp","created_at":1795213844,"online":false,"last_seen_at":1795213871,"version":"fake/0.8.0","accounts":[{"name":"personal","session_resets_at":null,"session_used_pct":38,"week_resets_at":null,"week_used_pct":61},{"name":"work","session_resets_at":null,"session_used_pct":92,"week_resets_at":null,"week_used_pct":70}],"accounts_at":1795213858}],"sessions":[{"id":"0196f3a0-0000-7000-8000-000000000001","runtime_id":"01a1fb15-5eea-78d0-a6b7-adbb1135747c","address":"taisce/main","fabric":"runtime-channel","project":"taisce","worktree":"main","branch":"agent-fabric","account":"personal","state":"needs_you","tool":"","note":"","last_line":"The tests pass. Shall I open the pull request?","context_used_pct":44,"since":1795213864,"updated_at":1795213864,"quiet_until":null},{"id":"0196f3a0-0000-7000-8000-000000000002","runtime_id":"01a1fb15-5eea-78d0-a6b7-adbb1135747c","address":"workbox/main","fabric":"runtime-channel","project":"workbox","worktree":"main","branch":"main","account":"work","state":"finished","tool":"","note":"","last_line":"Done: the hook receiver checks Host, Origin and the token.","context_used_pct":19,"since":1795213869,"updated_at":1795213869,"quiet_until":null},{"id":"0196f3a0-0000-7000-8000-000000000003","runtime_id":"01a1fb15-5eea-78d0-a6b7-adbb1135747c","address":"qompass/mcp-dx","fabric":"qompass","project":"qompass","worktree":"mcp-dx","branch":"mcp-dx","account":"work","state":"asleep","tool":"","note":"","last_line":"","context_used_pct":null,"since":1795213858,"updated_at":1795213858,"quiet_until":1795217471}]}
"""#

    /// `GET /api/fabric/runtimes`.
    static let runtimes = #"""
{"runtimes":[{"accounts":[{"name":"personal","session_resets_at":null,"session_used_pct":38,"week_resets_at":null,"week_used_pct":61},{"name":"work","session_resets_at":null,"session_used_pct":92,"week_resets_at":null,"week_used_pct":70}],"accounts_at":1795213858,"created_at":1795213844,"id":"01a1fb15-5eea-78d0-a6b7-adbb1135747c","last_seen_at":1795213871,"name":"mbp","online":false,"version":"fake/0.8.0"}]}
"""#

    /// `POST …/snooze`'s answer, and `DELETE …/snooze`'s.
    static let snoozed = #"""
{"id":"0196f3a0-0000-7000-8000-000000000002","runtime_id":"01a1fb15-5eea-78d0-a6b7-adbb1135747c","address":"workbox/main","fabric":"runtime-channel","project":"workbox","worktree":"main","branch":"main","account":"work","state":"finished","tool":"","note":"","last_line":"Done: the hook receiver checks Host, Origin and the token.","context_used_pct":19,"since":1795213869,"updated_at":1795213869,"quiet_until":1795217471}
"""#
    static let unsnoozed = #"""
{"id":"0196f3a0-0000-7000-8000-000000000002","runtime_id":"01a1fb15-5eea-78d0-a6b7-adbb1135747c","address":"workbox/main","fabric":"runtime-channel","project":"workbox","worktree":"main","branch":"main","account":"work","state":"finished","tool":"","note":"","last_line":"Done: the hook receiver checks Host, Origin and the token.","context_used_pct":19,"since":1795213869,"updated_at":1795213869,"quiet_until":null}
"""#

    /// `GET /api/fabric/stream`, byte for byte: `retry:`, the fleet, the
    /// runtime connecting, the script, two snoozes and a lift, the runtime
    /// going offline.
    static let stream = #"""
retry: 3000

id: 1
event: fleet
data: {"rev":1,"runtimes":[{"id":"01a1fb15-5eea-78d0-a6b7-adbb1135747c","name":"mbp","created_at":1795213844,"online":false,"last_seen_at":null,"version":"","accounts":[],"accounts_at":null}],"sessions":[]}

id: 2
event: runtime
data: {"id":"01a1fb15-5eea-78d0-a6b7-adbb1135747c","name":"mbp","created_at":1795213844,"online":true,"last_seen_at":1795213858,"version":"fake/0.8.0","accounts":[],"accounts_at":null}

id: 3
event: session
data: {"id":"0196f3a0-0000-7000-8000-000000000001","runtime_id":"01a1fb15-5eea-78d0-a6b7-adbb1135747c","address":"taisce/main","fabric":"runtime-channel","project":"taisce","worktree":"main","branch":"agent-fabric","account":"personal","state":"working","tool":"Bash","note":"","last_line":"","context_used_pct":41,"since":1795213858,"updated_at":1795213858,"quiet_until":null}

id: 4
event: session
data: {"id":"0196f3a0-0000-7000-8000-000000000002","runtime_id":"01a1fb15-5eea-78d0-a6b7-adbb1135747c","address":"workbox/main","fabric":"runtime-channel","project":"workbox","worktree":"main","branch":"main","account":"work","state":"idle","tool":"","note":"","last_line":"","context_used_pct":12,"since":1795213858,"updated_at":1795213858,"quiet_until":null}

id: 5
event: session
data: {"id":"0196f3a0-0000-7000-8000-000000000003","runtime_id":"01a1fb15-5eea-78d0-a6b7-adbb1135747c","address":"qompass/mcp-dx","fabric":"qompass","project":"qompass","worktree":"mcp-dx","branch":"mcp-dx","account":"work","state":"asleep","tool":"","note":"","last_line":"","context_used_pct":null,"since":1795213858,"updated_at":1795213858,"quiet_until":null}

id: 6
event: runtime
data: {"id":"01a1fb15-5eea-78d0-a6b7-adbb1135747c","name":"mbp","created_at":1795213844,"online":true,"last_seen_at":1795213858,"version":"fake/0.8.0","accounts":[{"name":"personal","session_resets_at":null,"session_used_pct":38,"week_resets_at":null,"week_used_pct":61},{"name":"work","session_resets_at":null,"session_used_pct":92,"week_resets_at":null,"week_used_pct":70}],"accounts_at":1795213858}

id: 7
event: session
data: {"id":"0196f3a0-0000-7000-8000-000000000002","runtime_id":"01a1fb15-5eea-78d0-a6b7-adbb1135747c","address":"workbox/main","fabric":"runtime-channel","project":"workbox","worktree":"main","branch":"main","account":"work","state":"working","tool":"Edit","note":"","last_line":"","context_used_pct":14,"since":1795213861,"updated_at":1795213861,"quiet_until":null}

id: 8
event: session
data: {"id":"0196f3a0-0000-7000-8000-000000000001","runtime_id":"01a1fb15-5eea-78d0-a6b7-adbb1135747c","address":"taisce/main","fabric":"runtime-channel","project":"taisce","worktree":"main","branch":"agent-fabric","account":"personal","state":"needs_you","tool":"","note":"","last_line":"The tests pass. Shall I open the pull request?","context_used_pct":44,"since":1795213864,"updated_at":1795213864,"quiet_until":null}

id: 9
event: session
data: {"id":"0196f3a0-0000-7000-8000-000000000002","runtime_id":"01a1fb15-5eea-78d0-a6b7-adbb1135747c","address":"workbox/main","fabric":"runtime-channel","project":"workbox","worktree":"main","branch":"main","account":"work","state":"finished","tool":"","note":"","last_line":"Done: the hook receiver checks Host, Origin and the token.","context_used_pct":19,"since":1795213869,"updated_at":1795213869,"quiet_until":null}

id: 10
event: session
data: {"id":"0196f3a0-0000-7000-8000-000000000002","runtime_id":"01a1fb15-5eea-78d0-a6b7-adbb1135747c","address":"workbox/main","fabric":"runtime-channel","project":"workbox","worktree":"main","branch":"main","account":"work","state":"finished","tool":"","note":"","last_line":"Done: the hook receiver checks Host, Origin and the token.","context_used_pct":19,"since":1795213869,"updated_at":1795213869,"quiet_until":1795217471}

id: 11
event: session
data: {"id":"0196f3a0-0000-7000-8000-000000000002","runtime_id":"01a1fb15-5eea-78d0-a6b7-adbb1135747c","address":"workbox/main","fabric":"runtime-channel","project":"workbox","worktree":"main","branch":"main","account":"work","state":"finished","tool":"","note":"","last_line":"Done: the hook receiver checks Host, Origin and the token.","context_used_pct":19,"since":1795213869,"updated_at":1795213869,"quiet_until":null}

id: 12
event: session
data: {"id":"0196f3a0-0000-7000-8000-000000000003","runtime_id":"01a1fb15-5eea-78d0-a6b7-adbb1135747c","address":"qompass/mcp-dx","fabric":"qompass","project":"qompass","worktree":"mcp-dx","branch":"mcp-dx","account":"work","state":"asleep","tool":"","note":"","last_line":"","context_used_pct":null,"since":1795213858,"updated_at":1795213858,"quiet_until":1795217471}

id: 13
event: runtime
data: {"id":"01a1fb15-5eea-78d0-a6b7-adbb1135747c","name":"mbp","created_at":1795213844,"online":false,"last_seen_at":1795213871,"version":"fake/0.8.0","accounts":[{"name":"personal","session_resets_at":null,"session_used_pct":38,"week_resets_at":null,"week_used_pct":61},{"name":"work","session_resets_at":null,"session_used_pct":92,"week_resets_at":null,"week_used_pct":70}],"accounts_at":1795213858}


"""#

    /// A second runtime removed while a stream was open.
    static let runtimeGone = #"""
id: 15
event: runtime_gone
data: {"id":"01a1fb16-29a2-75d1-9e5a-e9bdfc58ed09"}


"""#

    /// Typed from `fabric/routes.rs` (`change_event`): the hub sends this
    /// when a session is evicted or expires, which a short recording cannot
    /// make happen.
    static let sessionGone = "id: 14\nevent: session_gone\ndata: {\"id\":\"\(workbox)\",\"runtime_id\":\"\(runtime)\"}\n\n"

    static func data(_ s: String) -> Data { Data(s.utf8) }
}
