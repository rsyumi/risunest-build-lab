import Foundation

enum NativeFileStateError: LocalizedError {
    case publicationPending
    var errorDescription: String? { "Publication has not completed" }
}

struct NativeFileState {
    let staging: URL

    func receiptURL(_ id: String) throws -> URL {
        guard UUID(uuidString: id) != nil else { throw CocoaError(.validationMissingMandatoryProperty) }
        return staging.appendingPathComponent("receipts/\(id).json")
    }

    func acknowledge(_ id: String) throws {
        let receipt = try receiptURL(id)
        guard FileManager.default.fileExists(atPath: receipt.path) else { return }
        let value = try JSONSerialization.jsonObject(with: Data(contentsOf: receipt)) as? [String: Any]
        guard ["succeeded", "cancelled"].contains(value?["state"] as? String ?? "") else {
            throw NativeFileStateError.publicationPending
        }
        try FileManager.default.removeItem(at: receipt)
    }

    func complete(_ id: String, result: [String: Any]) throws {
        let receipt = try receiptURL(id)
        var terminal = result
        terminal["state"] = result["cancelled"] as? Bool == true ? "cancelled" : "succeeded"
        try JSONSerialization.data(withJSONObject: terminal).write(to: receipt, options: .atomic)
    }

    static func lifecycleScript(_ event: String, id: String) -> String? {
        guard let data = try? JSONSerialization.data(withJSONObject: ["event": event, "id": id]),
              let detail = String(data: data, encoding: .utf8) else { return nil }
        return "window.dispatchEvent(new CustomEvent('risunest-ios-lifecycle',{detail:\(detail)}))"
    }

    static func removeStagedInboxSource(_ source: URL, inbox: URL) throws {
        let owned = inbox.resolvingSymlinksInPath().standardizedFileURL
        let resolved = source.resolvingSymlinksInPath().standardizedFileURL
        guard source.isFileURL, resolved.deletingLastPathComponent() == owned else { return }
        let values = try source.resourceValues(forKeys: [.isRegularFileKey, .isSymbolicLinkKey])
        guard values.isRegularFile == true, values.isSymbolicLink != true else { return }
        try FileManager.default.removeItem(at: source)
    }
}
