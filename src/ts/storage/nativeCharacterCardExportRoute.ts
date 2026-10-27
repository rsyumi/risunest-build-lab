import { prepareCharacterPortraitRendition } from './characterPortraitRendition'
import { save } from './fileSelection'

import { isTauriIOS, isTauriAndroid, isTauriDesktop } from '../platform'
import type { CharacterDetail, DataRevision } from './persistentDataStore'
import { getPersistentDataStore } from './persistentDataStoreFactory'
import { readPinnedCharacterDetail } from './persistentRecordIterator'
import {
    runNativeCharacterCardExport,
    type NativeCharacterCardExportInput,
    type NativeFileJobOptions,
    type NativeFileJobResult,
} from './nativeFileJobs'
import { getPersistentDataRuntime } from './persistentDataRuntime.svelte'
import {
    runNativeContentExport,
    type NativeContentExportPickerDependencies,
} from './nativeContentExportPicker'

interface NativeCharacterCardPickerInput {
    characterId: string
    suggestedName: string
    format: 'json-card' | 'png-card'
    projectCharacter(character: CharacterDetail): Record<string, unknown>
}

interface NativeCharacterCardExportRuntime {
    readonly revision: number
    flushPendingData(reason: string): Promise<void>
}

interface NativeCharacterCardExportRouteDependencies {
    isDesktop(): boolean
    isAndroid(): boolean
    isIOS?(): boolean
    reserveAndroidDestination?: NativeContentExportPickerDependencies['reserveAndroidDestination']
    chooseDestination(
        suggestedName: string,
        format: NativeCharacterCardPickerInput['format'],
    ): Promise<string | null>
    runtime(): NativeCharacterCardExportRuntime
    readCharacter(
        characterId: string,
        revision: DataRevision,
    ): Promise<CharacterDetail>
    preparePortrait?: typeof prepareCharacterPortraitRendition
    runExport(
        input: NativeCharacterCardExportInput,
        options?: NativeFileJobOptions,
    ): Promise<NativeFileJobResult>
}

const productionDependencies: NativeCharacterCardExportRouteDependencies = {
    isDesktop: () => isTauriDesktop,
    isAndroid: () => isTauriAndroid,
    isIOS: () => isTauriIOS,
    chooseDestination: (suggestedName, format) =>
        save({
            defaultPath: suggestedName,
            filters: [
                {
                    name:
                        format === 'json-card'
                            ? 'JSON character card'
                            : 'PNG character card',
                    extensions: [format === 'json-card' ? 'json' : 'png'],
                },
            ],
        }),
    runtime: getPersistentDataRuntime,
    readCharacter: (characterId, revision) =>
        readPinnedCharacterDetail(
            getPersistentDataStore(),
            characterId,
            revision,
        ),
    preparePortrait: prepareCharacterPortraitRendition,
    runExport: runNativeCharacterCardExport,
}

export async function exportNativeCharacterCardFromPicker(
    input: NativeCharacterCardPickerInput,
    options: NativeFileJobOptions = {},
    dependencies: NativeCharacterCardExportRouteDependencies = productionDependencies,
): Promise<NativeFileJobResult | null | undefined> {
    return runNativeContentExport({
        suggestedName: input.suggestedName,
        flushReason: 'native-character-card-export',
        chooseDestination: () =>
            dependencies.chooseDestination(input.suggestedName, input.format),
    }, options, dependencies, 'dialog', async (flow, options) => {
        const character = await dependencies.readCharacter(input.characterId, flow.expectedRevision)
        const metadata = input.projectCharacter(character)
        const rendition = input.format === 'png-card'
            ? await dependencies.preparePortrait?.(character, flow.expectedRevision, 'png-card', options.signal) : undefined
        try {
            options.signal?.throwIfAborted()
            return await dependencies.runExport({
                characterId: input.characterId,
                destination: flow.destination,
                expectedRevision: flow.expectedRevision,
                ...(rendition ? { portraitRendition: rendition.descriptor } : {}),
                format: input.format,
                metadata,
            }, options)
        } finally { await rendition?.release() }
    })
}
