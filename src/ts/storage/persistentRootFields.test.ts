import { readFileSync } from 'node:fs'
import ts from 'typescript'
import { describe, expect, it } from 'vitest'
import { personaMirrorMap, presetMirrorMap } from './effectiveIdentityState'
import { sharedRootFields } from './persistentRootFields'

/** Kept on this installation only. */
const deviceLocalRootFields = [
    'loreBookPage', 'didFirstSetup', 'nanogptSubscriptionState', 'saveTime', 'lastPatchNoteCheckVersion',
    'statics', 'vertexAccessToken', 'vertexAccessTokenExpires', 'authRefreshes',
]

/** Recomputed from another value on every device, so a root unit would only race that value. */
const derivedRootFields = [
    // The selected preset's and persona's fields, rewritten after every receive.
    ...Object.keys(presetMirrorMap),
    ...Object.keys(personaMirrorMap),
    // translatorPresets[translatorPresetId], rewritten on load and whenever the current translator preset is read.
    'translatorPrompt', 'translatorMaxResponse',
    // Explicit variables combined with the bound conversation's toggles.
    'globalChatVariables',
    // The account session; the native root capture drops it.
    'account',
    // Raised to the current format on every load.
    'formatversion',
]

/** Synced through record, order, toggle, variable or protected-value units instead of a root unit. */
const ownUnitRootFields = [
    'characters', 'botPresets', 'personas', 'modules', 'plugins', 'loadouts', 'customModels',
    'pluginCustomStorage', 'pluginStorageMeta', 'characterOrder', 'protectedPresetValues', 'explicitGlobalChatVariables',
]

function databaseKeys(): string[] {
    const path = 'src/ts/storage/database.svelte.ts'
    const source = ts.createSourceFile(path, readFileSync(path, 'utf8'), ts.ScriptTarget.Latest, true)
    const declaration = source.statements.find((statement): statement is ts.InterfaceDeclaration =>
        ts.isInterfaceDeclaration(statement) && statement.name.text === 'Database')
    if (!declaration) throw new Error('Database interface not found')
    expect(declaration.heritageClauses).toBeUndefined()
    return declaration.members.map((member) => {
        if (!ts.isPropertySignature(member) || !(ts.isIdentifier(member.name) || ts.isStringLiteral(member.name))) {
            throw new Error(`Unclassifiable Database member: ${member.getText(source)}`)
        }
        return member.name.text
    })
}

function nativeRootFields(): string[] {
    const source = readFileSync('src-tauri/src/persistent_store/lww_classification.rs', 'utf8')
    const body = /pub\(super\) const ROOT_FIELDS: &\[&str\] = &\[([^\]]*)\];/.exec(source)?.[1]
    if (body === undefined) throw new Error('ROOT_FIELDS not found')
    return [...body.matchAll(/"([^"]+)"/g)].map((match) => match[1])
}

describe('root field classification', () => {
    it('gives every Database root key exactly one class', () => {
        const classes = new Map<string, string[]>()
        const assign = (keys: Iterable<string>, name: string) => {
            for (const key of keys) classes.set(key, [...(classes.get(key) ?? []), name])
        }
        assign(sharedRootFields, 'shared')
        assign(deviceLocalRootFields, 'device-local')
        assign(derivedRootFields, 'derived')
        assign(ownUnitRootFields, 'own unit')
        const keys = databaseKeys()
        expect(new Set(keys).size).toBe(keys.length)
        expect(keys.filter((key) => !classes.has(key))).toEqual([])
        expect([...classes].filter(([, names]) => names.length > 1)).toEqual([])
        expect([...classes.keys()].filter((key) => !keys.includes(key))).toEqual([])
    })

    it('shares every RisuNest root setting', () => {
        expect(databaseKeys().filter((key) => key.startsWith('risunest') && !sharedRootFields.has(key))).toEqual([])
        expect(sharedRootFields.has('risunestChatEditPopup')).toBe(true)
    })

    it('keeps the native root unit list equal to the shared root fields', () => {
        const native = nativeRootFields()
        expect(native).toEqual([...native].sort())
        expect(native).toEqual([...sharedRootFields].sort())
    })
})
