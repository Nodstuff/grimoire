import Foundation

/// Settings version 0: the guardrails and limits every runtime enforces
/// before a signed Settings reaches it, and what the editor starts from
/// when the server has none (`default-settings.json`, embedded).
public enum FabricDefaults {
    public static let settings: FabricSettings = {
        struct File: Decodable { var settings: FabricSettings }
        guard let url = Bundle.module.url(forResource: "default-settings", withExtension: "json"),
              let data = try? Data(contentsOf: url),
              let file = try? JSONDecoder().decode(File.self, from: data)
        else { return FabricSettings(version: 0, limits: FabricLimits.defaults) }
        return file.settings
    }()
}
