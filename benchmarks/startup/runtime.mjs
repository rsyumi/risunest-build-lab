import { copyFile, mkdir, readFile } from 'node:fs/promises'
import { constants } from 'node:fs'
import { createHash } from 'node:crypto'
import path from 'node:path'

export function resolveCargoTarget(repository, configured) {
    if (!configured || !path.isAbsolute(configured)) throw new Error('Set CARGO_TARGET_DIR to the absolute shared Cargo target before running')
    return path.resolve(configured)
}

export function syntheticProfile(identifier, localAppData) {
    if (!/^RisuNest\.phase3benchmark\.r[0-9a-f]{12}\.[a-zA-Z0-9]+$/.test(identifier)
        || !localAppData || !path.isAbsolute(localAppData)) throw new Error('Invalid synthetic profile inputs')
    const name = 'agent-startup-' + createHash('sha256').update(identifier).digest('hex').slice(0, 12)
    return { name, localAppData, data: path.join(localAppData, 'RisuNestProfiles-agent', name, 'data') }
}

export function assertAgentDataRoot(actual, profile) {
    if (!profile || !/^agent-startup-[0-9a-f]{12}$/.test(profile.name)) throw new Error('Invalid synthetic profile')
    const expected = path.join(profile.localAppData, 'RisuNestProfiles-agent', profile.name, 'data')
    if (typeof actual !== 'string' || path.resolve(actual).toLowerCase() !== path.resolve(expected).toLowerCase()) {
        throw new Error('Native synthetic data root mismatch')
    }
}

export async function copyBenchmarkBinary(target, name, temporary) {
    if (!/^[a-zA-Z0-9_-]+$/.test(name)) throw new Error('Invalid binary name')
    const source = path.join(target, 'release', name + '.exe')
    const sha256 = createHash('sha256').update(await readFile(source)).digest('hex')
    const directory = path.join(temporary, 'binary')
    await mkdir(directory, { recursive: true })
    const binary = path.join(directory, name + '-' + sha256 + '.exe')
    try { await copyFile(source, binary, constants.COPYFILE_EXCL) }
    catch (error) { if (error.code !== 'EEXIST') throw error }
    const copied = createHash('sha256').update(await readFile(binary)).digest('hex')
    if (copied !== sha256) throw new Error('Shared build artifact changed during private copy')
    return { binary, sha256 }
}
