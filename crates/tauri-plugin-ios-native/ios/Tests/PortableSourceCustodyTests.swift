import XCTest
import Darwin
@testable import tauri_plugin_ios_native

final class PortableSourceCustodyTests: XCTestCase {
    private var root: URL!
    override func setUpWithError() throws {
        root = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
    }
    override func tearDownWithError() throws { try FileManager.default.removeItem(at: root) }

    func testOrphanReceiptArrayEncodesNamesAndExplicitNull() throws {
        let receipts = [
            PortableSourceCustody.RetiredSource(token: "named-token", name: "synthetic.risunest"),
            PortableSourceCustody.RetiredSource(token: "unnamed-token", name: nil),
        ].map { $0.receipt }
        let data = try JSONEncoder().encode(receipts)
        let result = try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [[String: Any]])
        XCTAssertEqual(result.count, 2)
        XCTAssertEqual(Set(result[0].keys), ["token", "name"])
        XCTAssertEqual(result[0]["token"] as? String, "named-token")
        XCTAssertEqual(result[0]["name"] as? String, "synthetic.risunest")
        XCTAssertEqual(Set(result[1].keys), ["token", "name"])
        XCTAssertEqual(result[1]["token"] as? String, "unnamed-token")
        XCTAssertTrue(result[1]["name"] is NSNull)
    }

    private func select(_ custody: PortableSourceCustody, owner: String, name: String = "synthetic.risunest") throws -> String {
        let file = root.appendingPathComponent(name)
        try Data(repeating: 7, count: 131072).write(to: file)
        let ready = expectation(description: "selected descriptor")
        var result: Result<[String: Any], Error>?
        custody.select(file, owner: owner) { result = $0; ready.fulfill() }
        wait(for: [ready], timeout: 5)
        return try XCTUnwrap(result).get()["token"] as! String
    }

    func testNewPickerCannotDiscardActiveSelectionAndResetRetiresOnlyItsActualOwner() throws {
        let custody = PortableSourceCustody()
        let token = try select(custody, owner: "active-owner")
        let rejected = expectation(description: "concurrent picker refused")
        custody.select(root.appendingPathComponent("synthetic.risunest"), owner: "new-owner") { result in
            if case .success = result { XCTFail("Concurrent selection replaced an active source") }
            rejected.fulfill()
        }
        wait(for: [rejected], timeout: 5)
        custody.retireUnclaimed(owner: "another-owner")
        XCTAssertNoThrow(try custody.descriptor(token: token, jobId: nil))
        XCTAssertTrue(custody.orphanReceipts().map { $0.token }.isEmpty)
        let retired = expectation(description: "retired scope finished")
        custody.retireUnclaimed(owner: "active-owner") { retired.fulfill() }
        wait(for: [retired], timeout: 5)
        XCTAssertThrowsError(try custody.descriptor(token: token, jobId: nil))
        XCTAssertEqual(custody.orphanReceipts().map { $0.token }, [token])
        XCTAssertTrue(custody.release(token: token, jobId: nil))
    }

    func testClaimedDescriptorAndScopeSurviveOwnerResetUntilExactJobReleaseCompletes() throws {
        let custody = PortableSourceCustody()
        let token = try select(custody, owner: "old-owner")
        let job = UUID().uuidString
        try custody.confirm(token: token, format: "portable")
        XCTAssertNoThrow(try custody.descriptor(token: token, jobId: job))
        custody.retireUnclaimed(owner: "old-owner")
        XCTAssertTrue(custody.orphanReceipts().map { $0.token }.isEmpty)
        XCTAssertFalse(custody.release(token: token, jobId: nil))
        XCTAssertFalse(custody.release(token: token, jobId: UUID().uuidString))
        XCTAssertNoThrow(try custody.descriptor(token: token, jobId: job))
        XCTAssertTrue(custody.release(token: token, jobId: job))
        XCTAssertThrowsError(try custody.descriptor(token: token, jobId: job))
    }

    func testOnlyConfirmedUpstreamSourceCanCopyTheHeldDescriptor() throws {
        let custody = PortableSourceCustody()
        let token = try select(custody, owner: "owner")
        try custody.confirm(token: token, format: "portable")
        XCTAssertThrowsError(try custody.materialize(token: token, staging: root))
        XCTAssertEqual(try FileManager.default.contentsOfDirectory(atPath: root.path).count, 1)
        try custody.confirm(token: token, format: "block-risu-save")
        let copied = try custody.materialize(token: token, staging: root)
        XCTAssertEqual(copied["bytes"] as? UInt64, 131072)
        XCTAssertEqual(try Data(contentsOf: URL(fileURLWithPath: copied["path"] as! String)), Data(repeating: 7, count: 131072))
        XCTAssertTrue(custody.release(token: token, jobId: nil))
    }

    func testRetiredOwnerWaitsForItsExactProbeAndRefusesNewClaim() throws {
        let custody = PortableSourceCustody()
        let token = try select(custody, owner: "old-owner")
        try custody.confirm(token: token, format: "portable")
        let probe = UUID().uuidString
        try custody.beginProbe(token: token, probeId: probe)
        let retired = expectation(description: "scope retired after metadata read")
        custody.retireUnclaimed(owner: "old-owner") { retired.fulfill() }
        XCTAssertThrowsError(try custody.descriptor(token: token, jobId: UUID().uuidString))
        XCTAssertFalse(custody.endProbe(token: token, probeId: UUID().uuidString))
        XCTAssertTrue(custody.endProbe(token: token, probeId: probe))
        wait(for: [retired], timeout: 5)
        XCTAssertEqual(custody.orphanReceipts().map { $0.token }, [token])
        XCTAssertThrowsError(try custody.descriptor(token: token, jobId: nil))
    }

    func testLateSelectionCompletionRetainsOnlyDisplayNameAndRetiresOnce() throws {
        let custody = PortableSourceCustody()
        let file = root.appendingPathComponent("late-synthetic.risunest")
        try Data([1, 2, 3]).write(to: file)
        let selected = expectation(description: "late selection callback")
        var token: String?
        custody.select(file, owner: "retiring-owner") { result in
            token = try? result.get()["token"] as? String
            custody.retireUnclaimed(owner: "retiring-owner")
            custody.retireUnclaimed(owner: "retiring-owner")
            selected.fulfill()
        }
        wait(for: [selected], timeout: 5)
        let receipt = try XCTUnwrap(custody.orphanReceipts().first)
        XCTAssertEqual(custody.orphanReceipts().count, 1)
        XCTAssertEqual(receipt.token, token)
        XCTAssertEqual(receipt.name, "late-synthetic.risunest")
        XCTAssertFalse(receipt.name!.contains(root.path))
        XCTAssertTrue(custody.acknowledgeOrphan(token: receipt.token))
        XCTAssertTrue(custody.orphanReceipts().isEmpty)
        XCTAssertFalse(custody.acknowledgeOrphan(token: receipt.token))
    }

    func testUnconsumedRegisteredOrphanSurvivesMoreThan32UnregisteredRetirements() throws {
        let custody = PortableSourceCustody()
        let old = try select(custody, owner: "registered-owner")
        let descriptor = try custody.descriptor(token: old, jobId: nil)
        var nativeDuplicate = Darwin.dup(descriptor["fd"] as! Int32)
        XCTAssertGreaterThanOrEqual(nativeDuplicate, 0)
        defer { if nativeDuplicate >= 0 { Darwin.close(nativeDuplicate) } }
        custody.retireUnclaimed(owner: "registered-owner")
        XCTAssertEqual(custody.orphanReceipts().map { $0.token }, [old])
        var expected = [old]
        for index in 0..<40 {
            let owner = "unregistered-owner-\(index)"
            let token = try select(custody, owner: owner, name: "synthetic-\(index).risunest")
            expected.append(token)
            custody.retireUnclaimed(owner: owner)
            XCTAssertEqual(custody.orphanReceipts().map { $0.token }, expected)
        }
        var info = stat()
        XCTAssertEqual(fstat(nativeDuplicate, &info), 0)
        XCTAssertEqual(custody.orphanReceipts().map { $0.token }, expected)
        XCTAssertTrue(custody.release(token: old, jobId: nil))
        XCTAssertFalse(custody.acknowledgeOrphan(token: UUID().uuidString))
        XCTAssertEqual(custody.orphanReceipts().map { $0.token }, expected)
        // Native calls ACK only after its duplicated descriptor is released.
        XCTAssertEqual(Darwin.close(nativeDuplicate), 0)
        nativeDuplicate = -1
        XCTAssertTrue(custody.acknowledgeOrphan(token: old))
        XCTAssertEqual(custody.orphanReceipts().map { $0.token }, Array(expected.dropFirst()))
        XCTAssertEqual(custody.orphanReceipts().map { $0.name }, (0..<40).map { "synthetic-\($0).risunest" })
        XCTAssertFalse(custody.acknowledgeOrphan(token: old))
    }
}
