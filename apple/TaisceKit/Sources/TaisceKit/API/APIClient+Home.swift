import Foundation

/// `POST /api/ask`: the answer doc the server wrote under Answers, or
/// nothing when no doc could ground one.
public struct AskAnswer: Codable, Sendable, Hashable {
    /// nil: nothing in the vault matched (no doc was made)
    public var docID: DocID?
    public var title: String
    /// cited blocks
    public var sources: Int
    /// docs they came from
    public var docs: Int

    public init(docID: DocID?, title: String, sources: Int, docs: Int) {
        self.docID = docID
        self.title = title
        self.sources = sources
        self.docs = docs
    }

    enum CodingKeys: String, CodingKey {
        case title, sources, docs
        case docID = "doc_id"
    }
}

extension APIClient {
    /// Ask the vault. Waits for the model (up to two minutes).
    public func ask(_ question: String) async throws -> AskAnswer {
        struct Body: Encodable { var question: String }
        var r = try await request("/api/ask", method: "POST")
        r.setValue("application/json", forHTTPHeaderField: "Content-Type")
        r.httpBody = try JSONEncoder().encode(Body(question: question))
        r.timeoutInterval = 150
        return try await send(r)
    }
}
