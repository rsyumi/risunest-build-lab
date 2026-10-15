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
        XCTAssertFalse(custody.cancelCopy(token: token, jobId: UUID().uuidString))
        var progress = [PortableSourceCopyProgress]()
        let copied = try custody.materialize(token: token, staging: root) { progress.append($0) }
        XCTAssertEqual(copied["bytes"] as? UInt64, 131072)
        XCTAssertEqual(progress.last?.copiedBytes, 131072)
        XCTAssertEqual(progress.last?.totalBytes, 131072)
        XCTAssertEqual(try Data(contentsOf: URL(fileURLWithPath: copied["path"] as! String)), Data(repeating: 7, count: 131072))
        XCTAssertTrue(custody.release(token: token, jobId: nil))
    }

    private func assertCopyFails(
        _ custody: PortableSourceCustody, _ token: String, with expected: PortableSourceError,
        onProgress: @escaping (PortableSourceCopyProgress) -> Void = { _ in }, file: StaticString = #filePath, line: UInt = #line
    ) {
        XCTAssertThrowsError(try custody.materialize(token: token, staging: root, onProgress: onProgress), file: file, line: line) { error in
            XCTAssertEqual(error as? PortableSourceError, expected, file: file, line: line)
        }
    }

    private func assertOnlySourceRemains(file: StaticString = #filePath, line: UInt = #line) throws {
        XCTAssertEqual(try FileManager.default.contentsOfDirectory(atPath: root.path), ["synthetic.risunest"], file: file, line: line)
        XCTAssertEqual(try Data(contentsOf: root.appendingPathComponent("synthetic.risunest")), Data(repeating: 7, count: 131072), file: file, line: line)
    }

    func testCopyCancelledBeforeOrDuringTheCopyRemovesPartialFiles() throws {
        for cancelledAt: UInt64 in [0, 65536] {
            let custody = PortableSourceCustody()
            let token = try select(custody, owner: "owner")
            try custody.confirm(token: token, format: "local-backup")
            if cancelledAt == 0 { XCTAssertTrue(custody.cancelCopy(token: token, jobId: nil)) }
            var reported = [UInt64]()
            assertCopyFails(custody, token, with: cancelledAt == 0 ? .ownership : .cancelled) { progress in
                reported.append(progress.copiedBytes)
                if progress.copiedBytes == cancelledAt { XCTAssertTrue(custody.cancelCopy(token: token, jobId: nil)) }
            }
            // The copy stops at the chunk where it was cancelled.
            XCTAssertEqual(reported, cancelledAt == 0 ? [] : [cancelledAt])
            XCTAssertTrue(custody.release(token: token, jobId: nil))
            try assertOnlySourceRemains()
        }
    }

    func testCopyCancelledAfterItsLastByteIsNotSynchronized() throws {
        var synchronized = false
        let custody = PortableSourceCustody(synchronizeCopy: { _ in synchronized = true })
        let token = try select(custody, owner: "owner")
        try custody.confirm(token: token, format: "local-backup")
        assertCopyFails(custody, token, with: .cancelled) { progress in
            if progress.copiedBytes == progress.totalBytes { XCTAssertTrue(custody.cancelCopy(token: token, jobId: nil)) }
        }
        XCTAssertFalse(synchronized)
        XCTAssertTrue(custody.release(token: token, jobId: nil))
        try assertOnlySourceRemains()
    }

    func testCopyCancelledWhileSynchronizingRemovesTheFinishedFile() throws {
        var custody: PortableSourceCustody!
        var token = ""
        custody = PortableSourceCustody(synchronizeCopy: { output in
            try output.synchronize()
            XCTAssertTrue(custody.cancelCopy(token: token, jobId: nil))
        })
        token = try select(custody, owner: "owner")
        try custody.confirm(token: token, format: "block-risu-save")
        assertCopyFails(custody, token, with: .cancelled)
        XCTAssertTrue(custody.release(token: token, jobId: nil))
        try assertOnlySourceRemains()
    }

    func testCopyFailuresReportStableCodes() throws {
        let custody = PortableSourceCustody()
        let unconfirmed = try select(custody, owner: "owner")
        try custody.confirm(token: unconfirmed, format: "portable")
        assertCopyFails(custody, unconfirmed, with: .ownership)
        try custody.confirm(token: unconfirmed, format: "local-backup")
        _ = try custody.materialize(token: unconfirmed, staging: root)
        assertCopyFails(custody, unconfirmed, with: .busy)
        XCTAssertTrue(custody.release(token: unconfirmed, jobId: nil))
        try assertOnlySourceRemains()

        let changed = try select(custody, owner: "owner")
        try custody.confirm(token: changed, format: "local-backup")
        let writer = try FileHandle(forWritingTo: root.appendingPathComponent("synthetic.risunest"))
        try writer.truncate(atOffset: 1)
        try writer.close()
        assertCopyFails(custody, changed, with: .changed)
        XCTAssertTrue(custody.release(token: changed, jobId: nil))

        // JavaScript maps these codes to the same dialog reasons on Android and iOS.
        XCTAssertEqual(
            [PortableSourceError.ownership, .busy, .changed, .cancelled, .readFailed, .writeFailed, .storageFull].map { $0.copyFailureCode },
            ["source-reselect-required", "source-busy", "source-changed", "cancelled", "source-read-failed", "spool-write-failed", "storage-full"]
        )
    }

    func testFullStorageDuringCopyDoesNotPublishAndRemovesItsPartialFile() throws {
        for error in [NSError(domain: NSPOSIXErrorDomain, code: Int(ENOSPC)),
                      NSError(domain: NSCocoaErrorDomain, code: NSFileWriteOutOfSpaceError)] {
            let custody = PortableSourceCustody(synchronizeCopy: { _ in throw error })
            let token = try select(custody, owner: "owner")
            try custody.confirm(token: token, format: "local-backup")
            assertCopyFails(custody, token, with: .storageFull)
            XCTAssertTrue(custody.release(token: token, jobId: nil))
            try assertOnlySourceRemains()
        }
    }

    func testReleaseReturnsWhileCopyIsStalledAndRemovesTheLateCopy() throws {
        let stalled = expectation(description: "copy waiting for provider")
        let settled = expectation(description: "cancelled copy cleaned")
        let unblock = DispatchSemaphore(value: 0)
        let custody = PortableSourceCustody(synchronizeCopy: { _ in stalled.fulfill(); unblock.wait() })
        let token = try select(custody, owner: "owner")
        try custody.confirm(token: token, format: "local-backup")
        let staging = root!
        DispatchQueue.global().async {
            XCTAssertThrowsError(try custody.materialize(token: token, staging: staging)) { error in
                XCTAssertEqual(error as? PortableSourceError, .cancelled)
            }
            settled.fulfill()
        }
        wait(for: [stalled], timeout: 5)
        let released = expectation(description: "release does not wait for provider")
        DispatchQueue.global().async {
            XCTAssertTrue(custody.release(token: token, jobId: nil))
            released.fulfill()
        }
        wait(for: [released], timeout: 5)
        unblock.signal()
        wait(for: [settled], timeout: 5)
        try assertOnlySourceRemains()
    }

    func testOwnerResetRemovesItsDeliveredCopyAndPreservesTheSelectedSource() throws {
        let custody = PortableSourceCustody()
        let token = try select(custody, owner: "old-page")
        try custody.confirm(token: token, format: "block-risu-save")
        _ = try custody.materialize(token: token, staging: root)
        custody.retireUnclaimed(owner: "unrelated-page")
        XCTAssertTrue(custody.orphanReceipts().isEmpty)
        XCTAssertEqual(try FileManager.default.contentsOfDirectory(atPath: root.path).count, 2)
        XCTAssertThrowsError(try custody.confirm(token: token, format: "portable"))
        XCTAssertThrowsError(try custody.beginProbe(token: token, probeId: UUID().uuidString))
        custody.retireUnclaimed(owner: "old-page")
        XCTAssertEqual(custody.orphanReceipts().map { $0.token }, [token])
        try assertOnlySourceRemains()
    }

    func testReleaseReportsCopyCleanupFailureAfterReleasingTheSource() throws {
        let custody = PortableSourceCustody()
        let token = try select(custody, owner: "owner")
        try custody.confirm(token: token, format: "local-backup")
        _ = try custody.materialize(token: token, staging: root)
        try FileManager.default.setAttributes([.posixPermissions: 0o500], ofItemAtPath: root.path)
        defer { try? FileManager.default.setAttributes([.posixPermissions: 0o700], ofItemAtPath: root.path) }
        XCTAssertFalse(custody.release(token: token, jobId: nil))
        XCTAssertThrowsError(try custody.descriptor(token: token, jobId: nil))
        XCTAssertTrue(FileManager.default.fileExists(atPath: root.appendingPathComponent("synthetic.risunest").path))
    }

    func testCopyProgressThrottlesLargeByteCountsAndAlwaysSendsCompletion() {
        var time: TimeInterval = 10
        var progress = [PortableSourceCopyProgress]()
        let reporter = PortableSourceCopyProgressReporter(now: { time }, emit: { progress.append($0) })
        let gib: UInt64 = 1024 * 1024 * 1024
        reporter.report(copied: gib, total: 8 * gib)
        time = 10.1
        reporter.report(copied: 2 * gib, total: 8 * gib)
        time = 10.25
        reporter.report(copied: 4 * gib, total: 8 * gib)
        reporter.report(copied: 4 * gib, total: 8 * gib)
        reporter.report(copied: 3 * gib, total: 8 * gib)
        reporter.report(copied: 8 * gib, total: 8 * gib)
        XCTAssertEqual(progress.map { $0.copiedBytes }, [gib, 4 * gib, 8 * gib])
        XCTAssertEqual(progress.map { $0.totalBytes }, [8 * gib, 8 * gib, 8 * gib])
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
