import assert from 'node:assert/strict'
import { spawnSync } from 'node:child_process'
import { copyFileSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

const root = mkdtempSync(join(tmpdir(), 'risunest-portable-source-'))
const sources = join(root, 'Sources/tauri-plugin-ios-native')
const tests = join(root, 'Tests/NativeFileStateTests')
mkdirSync(sources, { recursive: true })
mkdirSync(tests, { recursive: true })
const source = join(sources, 'PortableSourceCustody.swift')
copyFileSync(new URL('../crates/tauri-plugin-ios-native/ios/Sources/PortableSourceCustody.swift', import.meta.url), source)
copyFileSync(new URL('../crates/tauri-plugin-ios-native/ios/Tests/PortableSourceCustodyTests.swift', import.meta.url), join(tests, 'PortableSourceCustodyTests.swift'))
writeFileSync(join(root, 'Package.swift'), `// swift-tools-version:5.3
import PackageDescription
let package = Package(
    name: "PortableSourceContracts",
    platforms: [.macOS("14.0")],
    targets: [
        .target(name: "tauri-plugin-ios-native"),
        .testTarget(name: "NativeFileStateTests", dependencies: ["tauri-plugin-ios-native"])
    ]
)
`)
function run(filter, fails = false) {
    const result = spawnSync('swift', ['test', '--package-path', root, '--filter', filter], {
        encoding: 'utf8', timeout: 300_000, maxBuffer: 8 * 1024 * 1024,
    })
    const output = (result.stdout ?? '') + (result.stderr ?? '')
    process.stdout.write(output)
    assert.ifError(result.error)
    assert.match(output, /Executed [1-9]\d* tests?, with \d+ failures?/)
    if (fails) {
        assert.notEqual(result.status, 0, 'Mutation was not detected')
        assert.match(output, /Executed [1-9]\d* tests?, with [1-9]\d* failures?/)
    } else assert.equal(result.status, 0, 'Swift contracts failed')
}
try {
    run('PortableSourceCustodyTests')
    const original = readFileSync(source, 'utf8')
    for (const [before, after, test] of [
        ['source.copyCancelled = true', 'source.copyCancelled = false', 'testCopyCancellationBeforeDuringAndAtCompletionRemovesPartialFiles'],
        ['time - previous < 0.25', 'time - previous < 0.0', 'testCopyProgressThrottlesLargeByteCountsAndAlwaysSendsCompletion'],
        ['progress.report(copied: copied, total: source.bytes)', '_ = progress', 'testOnlyConfirmedUpstreamSourceCanCopyTheHeldDescriptor'],
    ]) {
        assert.equal(original.split(before).length, 2, 'Mutation must match exactly once')
        writeFileSync(source, original.replace(before, after))
        try { run(`PortableSourceCustodyTests/${test}`, true) }
        finally { writeFileSync(source, original) }
    }
    console.log('Portable source contracts and mutation checks passed')
} finally {
    rmSync(root, { recursive: true, force: true })
}
