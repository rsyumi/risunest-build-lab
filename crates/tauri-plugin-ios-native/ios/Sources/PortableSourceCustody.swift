import Foundation
import Darwin

enum PortableSourceError: Error {
    case busy, unavailable, notSeekable, ownership, changed, cancelled, readFailed, writeFailed

    /// The code a failed backup source copy reports to JavaScript. Android reports the same codes.
    var copyFailureCode: String {
        switch self {
        case .busy: return "source-busy"
        case .changed: return "source-changed"
        case .cancelled: return "cancelled"
        case .readFailed: return "source-read-failed"
        case .writeFailed: return "spool-write-failed"
        case .unavailable, .notSeekable, .ownership: return "source-reselect-required"
        }
    }
}

private func copyStep<T>(_ failure: PortableSourceError, _ step: () throws -> T) throws -> T {
    do { return try step() } catch { throw failure }
}

struct PortableSourceCopyProgress: Encodable {
    let copiedBytes: UInt64
    let totalBytes: UInt64
}

final class PortableSourceCopyProgressReporter {
    private let now: () -> TimeInterval
    private let emit: (PortableSourceCopyProgress) -> Void
    private var lastBytes: UInt64 = 0
    private var lastSentAt: TimeInterval?
    init(now: @escaping () -> TimeInterval = { ProcessInfo.processInfo.systemUptime }, emit: @escaping (PortableSourceCopyProgress) -> Void) {
        self.now = now; self.emit = emit
    }
    func report(copied: UInt64, total: UInt64) {
        guard copied > lastBytes else { return }
        let time = now()
        if let previous = lastSentAt, time - previous < 0.25, copied != total { return }
        lastBytes = copied; lastSentAt = time
        emit(PortableSourceCopyProgress(copiedBytes: copied, totalBytes: total))
    }
}

final class PortableSourceCustody {
    static let shared = PortableSourceCustody()
    private final class Source {
        let handle: FileHandle
        let bytes: UInt64
        let name: String
        let identity: Identity
        let owner: String
        let release = DispatchSemaphore(value: 0)
        let finished = DispatchSemaphore(value: 0)
        var jobId: String?
        var claimRequest: String?
        var releasing = false
        var format: String?
        var copying = false
        var copyCancelled = false
        let copied = DispatchSemaphore(value: 0)
        var probeId: String?
        var probeFinished: DispatchSemaphore?
        var completedProbes: [String] = []
        init(handle: FileHandle, info: stat, name: String, owner: String) {
            self.handle = handle; self.bytes = UInt64(info.st_size)
            self.identity = Identity(info); self.name = name
            self.owner = owner
        }
    }
    private struct Identity: Equatable {
        let device: Int32; let inode: UInt64; let size: Int64; let mode: UInt16; let links: UInt16
        let modified: Int; let modifiedNs: Int; let changed: Int; let changedNs: Int
        init(_ info: stat) {
            device = info.st_dev; inode = info.st_ino; size = info.st_size; mode = info.st_mode; links = info.st_nlink
            modified = info.st_mtimespec.tv_sec; modifiedNs = info.st_mtimespec.tv_nsec
            changed = info.st_ctimespec.tv_sec; changedNs = info.st_ctimespec.tv_nsec
        }
    }
    private let lock = NSLock()
    private let synchronizeCopy: (FileHandle) throws -> Void
    init(synchronizeCopy: @escaping (FileHandle) throws -> Void = { try $0.synchronize() }) {
        self.synchronizeCopy = synchronizeCopy
    }
    private let coordinatorQueue: OperationQueue = {
        let queue = OperationQueue()
        queue.maxConcurrentOperationCount = 1
        queue.name = "io.github.rsyumi.risunest.portable-source"
        return queue
    }()
    private var active: [String: Source] = [:]
    private var reserved = false
    private var reservedOwner: String?
    private var retiredOwners = Set<String>()
    private var released: [String] = []
    struct RetiredSource: Equatable {
        let token: String
        let name: String?
        var receipt: [String: String?] { ["token": token, "name": name] }
    }
    private var retired: [RetiredSource] = []
    private var selectionFinished: DispatchGroup?
    private let retirementQueue: OperationQueue = {
        let queue = OperationQueue(); queue.maxConcurrentOperationCount = 1
        queue.name = "io.github.rsyumi.risunest.portable-source-retirement"; return queue
    }()

    func select(_ url: URL, owner: String, completion: @escaping (Result<[String: Any], Error>) -> Void) {
        lock.lock()
        guard !reserved else { lock.unlock(); completion(.failure(PortableSourceError.busy)); return }
        reserved = true
        reservedOwner = owner
        let selectionDone = DispatchGroup()
        selectionDone.enter()
        selectionFinished = selectionDone
        let selectionToken = UUID().uuidString.lowercased()
        lock.unlock()
        coordinatorQueue.addOperation {
            let scope = url.startAccessingSecurityScopedResource()
            var coordinationError: NSError?
            var selected = false
            var completionSent = false
            var selectedSource: Source?
            NSFileCoordinator().coordinate(readingItemAt: url, options: [], error: &coordinationError) { coordinated in
                do {
                    self.lock.lock(); let retired = self.retiredOwners.contains(owner); self.lock.unlock()
                    guard !retired else { throw PortableSourceError.ownership }
                    guard coordinated.isFileURL else { throw PortableSourceError.unavailable }
                    let fd = Darwin.open(coordinated.path, O_RDONLY | O_NOFOLLOW)
                    guard fd >= 0 else { throw PortableSourceError.unavailable }
                    let handle = FileHandle(fileDescriptor: fd, closeOnDealloc: true)
                    var info = stat()
                    guard fstat(fd, &info) == 0, (info.st_mode & S_IFMT) == S_IFREG, info.st_size > 0,
                          lseek(fd, 0, SEEK_SET) == 0 else { throw PortableSourceError.notSeekable }
                    let token = selectionToken
                    let source = Source(handle: handle, info: info, name: url.lastPathComponent, owner: owner)
                    self.lock.lock()
                    guard !self.retiredOwners.contains(owner) else { self.lock.unlock(); handle.closeFile(); throw PortableSourceError.ownership }
                    self.active[token] = source
                    self.lock.unlock()
                    selected = true
                    selectedSource = source
                    completionSent = true
                    completion(.success(["cancelled": false, "token": token, "name": url.lastPathComponent, "bytes": source.bytes]))
                    // The accessor and security grant cover every native archive read.
                    source.release.wait()
                    handle.closeFile()
                } catch { completionSent = true; completion(.failure(error)) }
            }
            if scope { url.stopAccessingSecurityScopedResource() }
            selectedSource?.finished.signal()
            if !selected {
                self.lock.lock()
                if self.retiredOwners.contains(owner) {
                    self.retired.append(RetiredSource(token: selectionToken, name: url.lastPathComponent.isEmpty ? nil : url.lastPathComponent))
                }
                self.reserved = false; self.reservedOwner = nil; self.retiredOwners.remove(owner)
                self.lock.unlock()
                if !completionSent {
                    if let error = coordinationError { completion(.failure(error)) }
                    else { completion(.failure(PortableSourceError.unavailable)) }
                }
            }
            selectionDone.leave()
        }
    }

    func descriptor(token: String, jobId: String?) throws -> [String: Any] {
        lock.lock(); defer { lock.unlock() }
        guard UUID(uuidString: token) != nil, let source = active[token], !source.releasing, !source.copying else { throw PortableSourceError.unavailable }
        guard source.jobId != nil || !retiredOwners.contains(source.owner) else { throw PortableSourceError.ownership }
        if let jobId = jobId {
            guard UUID(uuidString: jobId) != nil, source.probeId == nil, source.format == "portable",
                  source.jobId == nil || source.jobId == jobId else { throw PortableSourceError.ownership }
            source.claimRequest = jobId
        } else if source.jobId != nil { throw PortableSourceError.ownership }
        var info = stat()
        guard fstat(source.handle.fileDescriptor, &info) == 0, Identity(info) == source.identity else { throw PortableSourceError.changed }
        if let jobId = jobId { source.jobId = jobId }
        return ["fd": source.handle.fileDescriptor, "bytes": source.bytes]
    }

    func confirm(token: String, format: String) throws {
        lock.lock(); defer { lock.unlock() }
        guard let source = active[token], source.jobId == nil, source.claimRequest == nil, !retiredOwners.contains(source.owner), !source.releasing, !source.copying, source.probeId == nil,
              ["portable", "block-risu-save", "local-backup"].contains(format) else { throw PortableSourceError.ownership }
        source.format = format
    }

    func beginProbe(token: String, probeId: String) throws {
        lock.lock(); defer { lock.unlock() }
        guard UUID(uuidString: probeId) != nil, let source = active[token], source.jobId == nil,
              !source.releasing, !source.copying, source.probeId == nil, !retiredOwners.contains(source.owner) else { throw PortableSourceError.ownership }
        source.probeId = probeId
        source.probeFinished = DispatchSemaphore(value: 0)
    }

    func endProbe(token: String, probeId: String) -> Bool {
        lock.lock(); defer { lock.unlock() }
        guard let source = active[token] else { return false }
        if source.completedProbes.contains(probeId) { return true }
        guard source.probeId == probeId else { return false }
        source.probeId = nil
        source.probeFinished?.signal()
        source.probeFinished = nil
        source.completedProbes.append(probeId)
        if source.completedProbes.count > 8 { source.completedProbes.removeFirst(source.completedProbes.count - 8) }
        return true
    }

    func cancelCopy(token: String, jobId: String?) -> Bool {
        lock.lock(); defer { lock.unlock() }
        guard jobId == nil, let source = active[token], source.jobId == nil, source.claimRequest == nil,
              source.format == "block-risu-save" || source.format == "local-backup" else { return false }
        source.copyCancelled = true
        return true
    }

    private func checkCopyCancellation(_ source: Source) throws {
        lock.lock(); defer { lock.unlock() }
        guard !source.releasing, !source.copyCancelled else { throw PortableSourceError.cancelled }
    }

    /// Throws a `PortableSourceError`, whose `copyFailureCode` JavaScript receives.
    func materialize(token: String, staging: URL, onProgress: @escaping (PortableSourceCopyProgress) -> Void = { _ in }) throws -> [String: Any] {
        lock.lock()
        guard let source = active[token], source.jobId == nil, source.claimRequest == nil, !retiredOwners.contains(source.owner), !source.releasing, !source.copyCancelled, source.probeId == nil,
              source.format == "block-risu-save" || source.format == "local-backup" else {
            lock.unlock(); throw PortableSourceError.ownership
        }
        guard !source.copying else { lock.unlock(); throw PortableSourceError.busy }
        source.copying = true
        lock.unlock()
        let folder = staging.appendingPathComponent(UUID().uuidString, isDirectory: true)
        var success = false
        defer {
            if !success { try? FileManager.default.removeItem(at: folder) }
            source.copied.signal()
        }
        try copyStep(.writeFailed) { try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true) }
        let destination = folder.appendingPathComponent("source.risudat")
        guard FileManager.default.createFile(atPath: destination.path, contents: nil) else { throw PortableSourceError.writeFailed }
        let output = try copyStep(.writeFailed) { try FileHandle(forWritingTo: destination) }
        defer { output.closeFile() }
        var info = stat()
        guard fstat(source.handle.fileDescriptor, &info) == 0, Identity(info) == source.identity else { throw PortableSourceError.changed }
        try copyStep(.readFailed) { try source.handle.seek(toOffset: 0) }
        var copied: UInt64 = 0
        let progress = PortableSourceCopyProgressReporter(emit: onProgress)
        while copied < source.bytes {
            try checkCopyCancellation(source)
            let bytes = try copyStep(.readFailed) { try source.handle.read(upToCount: Int(min(65536, source.bytes - copied))) } ?? Data()
            guard !bytes.isEmpty else { throw PortableSourceError.changed }
            try copyStep(.writeFailed) { try output.write(contentsOf: bytes) }
            copied += UInt64(bytes.count)
            progress.report(copied: copied, total: source.bytes)
        }
        try checkCopyCancellation(source)
        guard fstat(source.handle.fileDescriptor, &info) == 0, Identity(info) == source.identity else { throw PortableSourceError.changed }
        try copyStep(.writeFailed) { try synchronizeCopy(output) }
        try checkCopyCancellation(source)
        success = true
        return ["cancelled": false, "path": destination.path, "name": source.name, "bytes": copied]
    }

    func release(token: String, jobId: String?) -> Bool {
        lock.lock()
        guard let source = active[token] else {
            let completed = released.contains(token) || retired.contains(where: { $0.token == token })
            lock.unlock()
            return completed
        }
        let requested = source.jobId == nil && source.claimRequest != nil && source.claimRequest == jobId
        guard (source.claimRequest == nil && source.jobId == jobId) || (jobId != nil && (source.jobId == jobId || requested)) else { lock.unlock(); return false }
        if source.releasing { lock.unlock(); return false }
        source.releasing = true
        let copying = source.copying
        let probe = source.probeFinished
        lock.unlock()
        probe?.wait()
        if copying { source.copied.wait() }
        source.release.signal()
        source.finished.wait()
        lock.lock()
        active.removeValue(forKey: token)
        released.append(token)
        if released.count > 16 { released.removeFirst(released.count - 16) }
        reserved = false
        reservedOwner = nil
        retiredOwners.remove(source.owner)
        lock.unlock()
        return true
    }

    func retireUnclaimed(owner: String, completion: (() -> Void)? = nil) {
        lock.lock()
        if reservedOwner == owner { retiredOwners.insert(owner) }
        let receipts = active.filter { $0.value.owner == owner && $0.value.jobId == nil && $0.value.claimRequest == nil }
            .map { RetiredSource(token: $0.key, name: $0.value.name.isEmpty ? nil : $0.value.name) }
        let pending = receipts.isEmpty && reservedOwner == owner && active.isEmpty ? selectionFinished : nil
        lock.unlock()
        retirementQueue.addOperation {
            pending?.wait()
            for receipt in receipts where self.release(token: receipt.token, jobId: nil) {
                self.lock.lock()
                if !self.retired.contains(where: { $0.token == receipt.token }) { self.retired.append(receipt) }
                self.lock.unlock()
            }
            completion?()
        }
    }

    func orphanReceipts() -> [RetiredSource] {
        retirementQueue.waitUntilAllOperationsAreFinished()
        lock.lock(); defer { lock.unlock() }; return retired
    }

    func acknowledgeOrphan(token: String) -> Bool {
        lock.lock(); defer { lock.unlock() }
        guard active[token] == nil, let index = retired.firstIndex(where: { $0.token == token }) else { return false }
        retired.remove(at: index)
        return true
    }
}
