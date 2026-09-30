import Foundation
import XCTest
@testable import tauri_plugin_ios_native

final class NativeFileStateTests: XCTestCase {
    private var root: URL!
    override func setUpWithError() throws {
        root = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: root.appendingPathComponent("receipts"), withIntermediateDirectories: true)
    }
    override func tearDownWithError() throws { try FileManager.default.removeItem(at: root) }

    func testTerminalAcknowledgement() throws {
        let state = NativeFileState(staging: root)
        for cancelled in [false, true] {
            let id = UUID().uuidString
            try state.complete(id, result: ["cancelled": cancelled])
            XCTAssertTrue(FileManager.default.fileExists(atPath: try state.receiptURL(id).path))
            try state.acknowledge(id)
            XCTAssertFalse(FileManager.default.fileExists(atPath: try state.receiptURL(id).path))
        }
    }
    func testPendingAndInvalidIdentifiersStayProtected() throws {
        let state = NativeFileState(staging: root)
        let id = UUID().uuidString
        let receipt = try state.receiptURL(id)
        let pending = Data("{\"state\":\"pending\"}".utf8)
        try pending.write(to: receipt)
        XCTAssertThrowsError(try state.acknowledge(id))
        XCTAssertThrowsError(try state.acknowledge("../outside"))
        let receipts = root.appendingPathComponent("receipts")
        let saved = root.appendingPathComponent("saved-receipts")
        try FileManager.default.moveItem(at: receipts, to: saved)
        try Data([1]).write(to: receipts)
        XCTAssertThrowsError(try state.complete(id, result: ["cancelled": false]))
        try FileManager.default.removeItem(at: receipts)
        try FileManager.default.moveItem(at: saved, to: receipts)
        XCTAssertEqual(try Data(contentsOf: receipt), pending)
    }
    func testLifecycleEventCarriesEscapedDetachedValues() throws {
        let script = try XCTUnwrap(NativeFileState.lifecycleScript("expired", id: "quote'\""))
        XCTAssertTrue(script.hasPrefix("window.dispatchEvent(new CustomEvent('risunest-ios-lifecycle',{detail:"))
        XCTAssertTrue(script.contains("\\\""))
    }
    func testOnlyDirectOwnedInboxFilesAreRemoved() throws {
        let inbox = root.appendingPathComponent("Inbox", isDirectory: true)
        try FileManager.default.createDirectory(at: inbox, withIntermediateDirectories: true)
        let owned = inbox.appendingPathComponent("synthetic.charx")
        let external = root.appendingPathComponent("external.charx")
        let link = inbox.appendingPathComponent("link.charx")
        try Data([1]).write(to: owned)
        try Data([2]).write(to: external)
        try FileManager.default.createSymbolicLink(at: link, withDestinationURL: external)
        try NativeFileState.removeStagedInboxSource(external, inbox: inbox)
        try NativeFileState.removeStagedInboxSource(link, inbox: inbox)
        XCTAssertTrue(FileManager.default.fileExists(atPath: external.path))
        XCTAssertTrue(FileManager.default.fileExists(atPath: link.path))
        try NativeFileState.removeStagedInboxSource(owned, inbox: inbox)
        XCTAssertFalse(FileManager.default.fileExists(atPath: owned.path))
    }
}
