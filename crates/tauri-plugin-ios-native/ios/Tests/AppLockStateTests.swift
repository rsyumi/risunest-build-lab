import Foundation
import XCTest
@testable import tauri_plugin_ios_native

final class AppLockStateTests: XCTestCase {
    func testUnlockedFrameCannotReplaceARequiredGateInTheSameEpoch() {
        var state = AppLockState()
        state.policy.gateRequired = true
        XCTAssertFalse(state.ready(epoch: state.epoch, locked: false))
        XCTAssertTrue(state.coverRequired)
        XCTAssertTrue(state.ready(epoch: state.epoch, locked: true))
        XCTAssertFalse(state.coverRequired)
    }

    func testDepartureAndReturnKeepCoverUntilCurrentFrameIsReady() {
        var state = AppLockState()
        state.policy.enabled = true
        state.policy.timing = "immediate"
        XCTAssertTrue(state.ready(epoch: 0, locked: false))
        XCTAssertFalse(state.coverRequired)
        XCTAssertTrue(state.depart(at: 100))
        XCTAssertTrue(state.coverRequired)
        XCTAssertFalse(state.ready(epoch: 1, locked: false))
        XCTAssertTrue(state.becomeActive())
        XCTAssertFalse(state.ready(epoch: 0, locked: false))
        XCTAssertFalse(state.ready(epoch: 1, locked: false))
        XCTAssertTrue(state.coverRequired)
        XCTAssertTrue(state.ready(epoch: 2, locked: true))
        XCTAssertFalse(state.coverRequired)
        XCTAssertTrue(state.policy.gateRequired)
    }

    func testPreviewWorksWithoutAppLockAndWaitsForTheReturningFrame() {
        var state = AppLockState()
        state.policy.hideRecentsPreview = true
        XCTAssertTrue(state.ready(epoch: 0, locked: false))
        XCTAssertTrue(state.depart(at: 50))
        XCTAssertTrue(state.coverRequired)
        XCTAssertTrue(state.becomeActive())
        XCTAssertTrue(state.coverRequired)
        XCTAssertTrue(state.ready(epoch: 2, locked: false))
        XCTAssertFalse(state.coverRequired)
        XCTAssertFalse(state.policy.enabled)
    }

    func testOneDepartureTimestampPerAwayPeriodAndSleepInclusiveInput() {
        var state = AppLockState()
        state.policy.enabled = true
        state.policy.timing = "after-1m"
        _ = state.ready(epoch: 0, locked: false)
        XCTAssertTrue(state.depart(at: 1_000))
        XCTAssertFalse(state.depart(at: 2_000))
        XCTAssertEqual(state.snapshot(at: 61_000).departedAtMs, 1_000)
        XCTAssertEqual(state.snapshot(at: 61_000).atMs, 61_000)
        XCTAssertEqual(state.epoch, 1)
        _ = state.becomeActive()
        _ = state.ready(epoch: 2, locked: false)
        XCTAssertTrue(state.depart(at: 62_000))
        XCTAssertEqual(state.snapshot(at: 62_000).departedAtMs, 62_000)
    }

    func testLaunchOnlyDoesNotInstallReturnCover() {
        var state = AppLockState()
        state.policy.enabled = true
        _ = state.ready(epoch: 0, locked: false)
        _ = state.depart(at: 10)
        XCTAssertFalse(state.coverRequired)
        _ = state.becomeActive()
        XCTAssertFalse(state.coverRequired)
    }
}
