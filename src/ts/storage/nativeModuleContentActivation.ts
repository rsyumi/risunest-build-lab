import { alertConfirm } from '../alert'
import { language } from '../../lang'
import { v4 } from 'uuid'

import type { RisuModule } from '../process/modules'
import { assertModuleMCPImportAllowed } from '../process/mcp/moduleImport'
import type { AssetAlias } from './persistentDataStore'
import type {
    PreparedNativeContent,
    PreparedNativeContentActivationLifecycle,
    PreparedNativeRisumContent,
    PreparedRisumOwnerHead,
} from './nativeFileJobs'
import { appendPersistentRootModule } from './persistentDataRuntime.svelte'
import { PersistentRootModuleAppendRejectedError } from './saveCoordinator'

export interface PreparedRootModuleAppend {
    module: RisuModule
    assetAliases: Extract<AssetAlias, { kind: 'asset' }>[]
    ownerHead: PreparedRisumOwnerHead
}

export interface NativeModuleContentActivationDependencies {
    confirmLowLevelAccess(): Promise<boolean>
    createId(): string
    append(input: PreparedRootModuleAppend, signal?: AbortSignal): Promise<void>
}

const productionDependencies: NativeModuleContentActivationDependencies = {
    confirmLowLevelAccess: () => alertConfirm(language.lowLevelAccessConfirm),
    createId: v4,
    append: appendPersistentRootModule,
}

function requireRisum(content: PreparedNativeContent): PreparedNativeRisumContent {
    if (content.format !== 'risu-module') throw new TypeError('Prepared content is not a RISUM module')
    return content as PreparedNativeRisumContent
}

// The module the native mapping produced, or undefined where the TS steps below run instead: on every decline, so a
// module that needs the low-level access confirmation or fails a check gets the TS dialog or error.
function nativeMappedModule(content: PreparedNativeRisumContent): RisuModule | undefined {
    const mapping = content.nativeMapping
    return mapping?.outcome === 'mapped' ? mapping.module as unknown as RisuModule : undefined
}

// The module the native conversion produced from the native character, or undefined where TS converts: on its decline,
// and wherever the TS mapping produced the character, which is not the one the native module was converted from.
function nativeConvertedModule(content: PreparedNativeContent, character: object): RisuModule | undefined {
    const mapping = content.nativeMapping
    const moduleMapping = content.nativeModuleMapping
    return mapping?.outcome === 'mapped' && 'character' in mapping && mapping.character === character &&
        moduleMapping?.outcome === 'mapped'
        ? moduleMapping.module as unknown as RisuModule
        : undefined
}

async function mapPreparedRisumModule(
    content: PreparedNativeRisumContent,
    dependencies: NativeModuleContentActivationDependencies,
): Promise<RisuModule | null> {
    const module = structuredClone(content.metadata) as unknown as RisuModule
    assertModuleMCPImportAllowed(module)
    if (module.lowLevelAccess && !(await dependencies.confirmLowLevelAccess()))
        return null

    module.id = dependencies.createId()
    if (Object.hasOwn(module, 'assets')) {
        if (
            !Array.isArray(module.assets) ||
            module.assets.length !== content.assets.length
        ) {
            throw new TypeError(
                'Prepared RISUM asset metadata does not match its descriptors',
            )
        }
        module.assets = module.assets.map((tuple, position) => {
            if (!Array.isArray(tuple) || tuple.length < 3) {
                throw new TypeError(
                    `Prepared RISUM asset tuple ${position} is invalid`,
                )
            }
            const retained = structuredClone(tuple)
            retained[1] = content.assets[position].logicalId
            return retained
        })
    }
    return module
}

export async function activatePreparedNativeModuleContent(
    prepared: PreparedNativeContent,
    lifecycle: PreparedNativeContentActivationLifecycle,
    dependencies: NativeModuleContentActivationDependencies = productionDependencies,
    signal?: AbortSignal,
): Promise<{ moduleId: string } | null> {
    if (prepared.format !== 'risu-module') {
        const [
            { convertCharacterToModule },
            {
                mapPreparedNativeContent,
                preparedAliases,
                prepareAdditionalAssetOwnerHead,
            },
        ] = await Promise.all([
            import('../interchangeability'),
            import('./nativeCharacterContentActivation'),
        ])
        const character = await mapPreparedNativeContent(
            prepared,
            undefined,
            signal,
            lifecycle,
        )
        if (!character) return null
        let module = nativeConvertedModule(prepared, character)
        if (!module) {
            module = convertCharacterToModule(character)
            module.id = dependencies.createId()
        }
        const assetAliases = preparedAliases(prepared)
        signal?.throwIfAborted()
        const head = await prepareAdditionalAssetOwnerHead(
            character,
            assetAliases,
            lifecycle.prepareOwnerManifestAndSeal,
        )
        try {
            signal?.throwIfAborted()
            await dependencies.append(
                {
                    module,
                    assetAliases,
                    ownerHead: {
                        present: true,
                        manifestHash: head.manifestHash!,
                        entryCount: head.entryCount,
                    },
                },
                signal,
            )
        } catch (error) {
            if (
                error instanceof PersistentRootModuleAppendRejectedError ||
                (signal?.aborted && error === signal.reason)
            ) {
                try {
                    await lifecycle.abortPreparedContent?.()
                } catch {}
            }
            throw error
        }
        return { moduleId: module.id }
    }
    const content = requireRisum(prepared)
    const module = nativeMappedModule(content) ?? await mapPreparedRisumModule(content, dependencies)
    if (!module) return null

    const assetAliasesByKey = new Map<
        string,
        Extract<AssetAlias, { kind: 'asset' }>
    >()
    for (const asset of content.assets) {
        const alias: Extract<AssetAlias, { kind: 'asset' }> = {
            kind: 'asset',
            key: asset.logicalId,
            objectHash: asset.objectHash,
            size: asset.byteSize,
            mime: asset.mime,
            name: asset.name,
            ext: asset.ext,
        }
        const previous = assetAliasesByKey.get(alias.key)
        if (
            previous &&
            (previous.objectHash !== alias.objectHash ||
                previous.size !== alias.size)
        ) {
            throw new TypeError(
                `Prepared RISUM alias ${alias.key} has conflicting payloads`,
            )
        }
        if (!previous) assetAliasesByKey.set(alias.key, alias)
    }
    const assetAliases = [...assetAliasesByKey.values()]
    if (!lifecycle.sealPreparedContent) {
        throw new TypeError(
            'Prepared RISUM content cannot seal its native roots',
        )
    }
    signal?.throwIfAborted()
    await lifecycle.sealPreparedContent()
    if (signal?.aborted) {
        try {
            await lifecycle.abortPreparedContent?.()
        } catch {}
        signal.throwIfAborted()
    }
    try {
        await dependencies.append(
            { module, assetAliases, ownerHead: content.ownerHead },
            signal,
        )
    } catch (error) {
        if (
            error instanceof PersistentRootModuleAppendRejectedError ||
            (signal?.aborted && error === signal.reason)
        ) {
            try {
                await lifecycle.abortPreparedContent?.()
            } catch {}
        }
        throw error
    }
    return { moduleId: module.id }
}
