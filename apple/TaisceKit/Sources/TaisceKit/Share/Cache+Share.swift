import Foundation

extension Cache {
    /// What a share snapshot is built from: the server's tree as cached,
    /// plus only THIS doc's still-pending proposes (never refused or failed
    /// writes, never another doc's). `unsent` says whether any were folded in.
    public func shareBlocks(for id: DocID) async throws -> (blocks: [Block], unsent: Bool) {
        let base = try await blocks(of: id).map(\.block)
        let pending = try await pendingOutbox().compactMap { e -> ProposeRequest? in
            guard e.path == "/api/propose", let body = e.body,
                  let req = try? JSONDecoder().decode(ProposeRequest.self, from: body), req.docID == id
            else { return nil }
            return req
        }
        guard !pending.isEmpty, let rec = try await doc(id), let epoch = rec.bodyEpoch else { return (base, false) }
        var editor = DocEditor(docID: id, baseEpoch: pending.first?.baseEpoch ?? epoch, blocks: base)
        editor.overlay(pending.flatMap { $0.ops.map(\.kind) })
        return (editor.ordered().map(\.block), true)
    }
}
