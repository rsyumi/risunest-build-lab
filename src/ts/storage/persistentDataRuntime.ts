import { canonicalJson, canonicalClone } from './saveCoordinatorHelpers'
import {claimCommittedWorkingSetRecovery, continueCommittedWorkingSetRefresh, hasRetryableCommittedWorkingSetContinuation, rebaseCommittedWorkingSetContinuation, registerCommittedWorkingSetContinuation} from './committedWorkingSetContinuation'
import { diffRootMutations } from './rootMutation'
import { generatingConversations } from './generatingConversationRegistry'
import { captureLwwWorkingSetBaseline, applyLwwWorkingSetUnits } from './lwwWorkingSetApply'
import { translatePersistentRootUnitIntents } from './persistentIdentityHooks'
import type { LwwStageReceive, LwwApplyResult, PersistentUnitMutation, ConversationMutation, WholeMessageIntent, GeneratingConversation } from './persistentDataStore'
import { isTauri } from '../platform'
import type { PersistenceCanonicalCapture } from './reactivePersistenceCapture.svelte'
import type { Chat, Database, Message, botPreset, character, groupChat } from './database.svelte'
import { removeGroupMemberReferences } from './groupMembership'
import {
    ActiveWorkingSet,
    type ActiveConversationViewportSourceListener,
    type CharacterActivationOptions,
    type ConversationPublicationOptions,
    type CompleteConversationLease,
    type SelectedConversationTarget,
    type WindowedChatListEditResult,
    type WindowedConversationMutationController,
} from './activeWorkingSet.svelte'
import type { ActiveConversationSession } from './activeConversationSession'
import type { ConversationViewportSource } from '../conversationViewportSource'
import type {
    CharacterDetail,
    DataRevision,
    PersistentDataStore,
    PersistentRevisionLease,
    PersistentRoot,
    PersistentConversationMetadata,
    PluginStorageMutation,
} from './persistentDataStore'
import { RevisionConflictError } from './persistentDataStore'
import {
    SaveCoordinator,
    PersistentMutationFencedError,
    type CharacterAdditionRequest,
    type OfficialRevisionPublisher,
    type PersistentPresetMutation,
    type PersistentPresetMutationResult,
    type PersistentCharacterDetailMutation,
    type PersistentCharacterMutationResult,
    type PersistentDatabaseMaterializationOptions,
    type PersistentDatabaseSnapshot,
    type PersistentMutationToken,
    type PersistentSelectedConversation,
    type PersistentCompleteCharacterMutation,
    type PersistentCompleteCharacterUpsert,
    type PersistentCompleteCharacterUpsertOptions,
    type PersistentConversationReplacementResult,
    type PersistentReplacementOptions,
    type PersistentScopedReplacementOptions,
    type SaveCoordinatorClock,
    type WindowedConversationPersistenceAuthority,
} from './saveCoordinator'
import {
    type WorkingSetResidencyRegistry,
    workingSetResidency,
} from './workingSetResidency'
import {
    createCatalogCharacterStub,
    getCatalogCharacterMetadata,
    hasIncompletePersistentWorkingSet,
    isCatalogCharacterStub,
    isWorkingSetCharacterStub,
    isCatalogPresetWorkingSet,
    patchWorkingSetCharacterDetail,
    patchWorkingSetRoot,
    projectPinnedScalableWorkingSet,
    projectScalableWorkingSetAtRevision,
} from './workingSetCatalog'
import {
    applyTargetedWorkingSetInvalidation,
    isProjectedWorkingSet,
    readWorkingSetChangeWindow,
} from './targetedWorkingSetInvalidation'
import {
    assertPinnedRevision,
    releasePersistentRevisionLease,
} from './persistentRecordIterator'
import {
    createConversationSummaryStubFromChat,
    isConversationSummaryStub,
} from './conversationResidency'
import { createMetadataOnlySelectedConversation, isMetadataOnlySelectedConversation } from './selectedConversationLifecycle'

type CompleteCharacter = character | groupChat
type RootDatabase = PersistentRoot

export type CommittedApplyOutcome = Readonly<{
    kind: 'committed'
    revision: DataRevision
    projection: 'applied' | 'refresh-required'
}>

export type ReplacementChangeSet = Readonly<{
    root: boolean
    presets: boolean
    pluginStorage: boolean
    characterIds: readonly string[]
    conversations: readonly Readonly<{ characterId: string; conversationId: string }>[]
    wholeLibrary: boolean
}>

export function capturePersistentRoot(database: Database): RootDatabase {
    const {
        characters: _characters,
        botPresets: _botPresets,
        pluginCustomStorage: _pluginCustomStorage,
        pluginStorageMeta: _pluginStorageMeta,
        ...root
    } = database
    if (isTauri) delete root.account
    const presetId = database.botPresets?.[database.botPresetsId]?.['id']
    const personaId = database.personas?.[database.selectedPersona]?.id
    return { ...root, botPresetsId: typeof presetId === 'string' ? presetId : root.botPresetsId,
        selectedPersona: typeof personaId === 'string' ? personaId : root.selectedPersona }
}

export function capturePersistentPluginStorage(
    database: Database,
): Database['pluginCustomStorage'] | null {
    if (isCatalogPresetWorkingSet(database.botPresets)) return null
    return Object.prototype.hasOwnProperty.call(database, 'pluginCustomStorage')
        ? database.pluginCustomStorage ?? {}
        : null
}

export function capturePersistentPresets(database: Database): botPreset[] | null {
    const presets = database.botPresets ?? []
    if (isCatalogPresetWorkingSet(presets)) return null
    return presets
}

export function publishPersistentConversationReplacementToWorkingSet(
    database: Database,
    result: PersistentConversationReplacementResult,
): void {
    const character = database.characters.find(
        (candidate) => candidate.chaId === result.characterId,
    )
    if (!character || isWorkingSetCharacterStub(character)) return
    const index = character.chats.findIndex(
        (candidate) => candidate.id === result.conversationId,
    )
    if (index < 0) return
    // A metadata-only selected-conversation shell is owned by the windowed
    // authority; swapping it for a full clone would defeat eviction and
    // desynchronize the windowed baseline. Rehydration picks up the durable
    // replacement instead.
    if (isMetadataOnlySelectedConversation(character.chats[index])) return
    character.chats[index] = isConversationSummaryStub(character.chats[index])
        ? createConversationSummaryStubFromChat(
            result.characterId,
            result.conversation,
            index,
        )
        : structuredClone(result.conversation)
}

export function captureSelectedPersistentCharacter(
    database: Database,
    selectedIndex: number,
): CompleteCharacter | null {
    return database.characters[selectedIndex] ?? null
}

export function restoreStableWorkingSetSelection(
    database: Database,
    characterId: string | null,
    conversationId: string | null,
    selectCharacterIndex: (index: number) => void,
): void {
    if (!characterId) {
        selectCharacterIndex(-1)
        return
    }
    const characterIndex = database.characters.findIndex(
        (candidate) => candidate.chaId === characterId,
    )
    if (characterIndex < 0) {
        selectCharacterIndex(-1)
        return
    }
    const character = database.characters[characterIndex]
    if (conversationId) {
        const conversationIndex = character.chats.findIndex(
            (candidate) => candidate.id === conversationId,
        )
        if (conversationIndex >= 0) character.chatPage = conversationIndex
    }
    selectCharacterIndex(characterIndex)
}

export function captureResidentPersistentCharacter(
    database: Database,
    id: string,
    residency: WorkingSetResidencyRegistry = workingSetResidency,
): CompleteCharacter | null {
    if (residency.isCharacterReleased(id)) return null
    const character = database.characters.find((candidate) => candidate.chaId === id) ?? null
    if (character && isWorkingSetCharacterStub(character)) return null
    return character
}

export function publishPersistentCharacterMutationToWorkingSet(
    database: Database,
    state: PersistentCharacterMutationResult,
    residency: WorkingSetResidencyRegistry,
    selectedIndex: number,
    selectCharacterIndex: (index: number) => void,
): void {
    patchWorkingSetRoot(database, state.root)
    for (const detail of state.relatedCharacters ?? []) {
        const relatedIndex = database.characters.findIndex(
            (candidate) => candidate.chaId === detail.chaId,
        )
        if (relatedIndex >= 0) {
            patchWorkingSetCharacterDetail(database.characters[relatedIndex], detail)
        }
    }
    const index = database.characters.findIndex(
        (candidate) => candidate.chaId === state.characterId,
    )
    if (state.kind === 'delete') {
        const selected = database.characters[selectedIndex]
        if (
            selected?.type === 'group' &&
            Array.isArray(selected.characters) &&
            selected.chaId !== state.characterId
        ) {
            const retained = removeGroupMemberReferences(
                selected,
                new Set([state.characterId]),
            )
            selected.characters = retained.characters
            selected.characterTalks = retained.characterTalks
            selected.characterActive = retained.characterActive
        }
        if (index >= 0) database.characters.splice(index, 1)
        residency.forgetCharacter(state.characterId)
        if (selectedIndex === index) selectCharacterIndex(-1)
        else if (index >= 0 && selectedIndex > index) selectCharacterIndex(selectedIndex - 1)
        return
    }
    if (!state.character) return
    if (state.kind === 'detail') {
        if (index >= 0) patchWorkingSetCharacterDetail(database.characters[index], state.character)
        return
    }

    const complete = state.character as CompleteCharacter
    const keepBounded = residency.allowsEviction && (
        state.kind === 'add' ||
        index < 0 ||
        isCatalogCharacterStub(database.characters[index]) ||
        residency.isCharacterReleased(state.characterId)
    )
    if (keepBounded) {
        const configuredIndex = index >= 0
            ? getCatalogCharacterMetadata(database.characters[index])?.configuredIndex ?? index
            : database.characters.length
        const stub = createCatalogCharacterStub({
            id: complete.chaId,
            name: complete.name,
            image: complete.image,
            configuredIndex,
            recentAt: complete.lastInteraction ?? 0,
            trashed: complete.trashTime !== undefined,
            conversationCount: complete.chats.length,
            type: complete.type,
            creatorNotes: complete.creatorNotes,
            trashTime: complete.trashTime,
        })
        if (index < 0) database.characters.push(stub)
        else database.characters[index] = stub
        residency.markCharacterReleased(state.characterId)
        return
    }
    if (index < 0) database.characters.push(complete)
    else database.characters[index] = complete
    residency.markCharacterHydrated(state.characterId)
}

export interface PersistentDataRuntimeStateAdapter {
    captureCharacters?(): readonly CompleteCharacter[]
    captureCharacterIndex?(): ReadonlyMap<string, CompleteCharacter>
    beforeCapture?(): void
    afterRemoteApply?(): void
    canonicalCapture?: PersistenceCanonicalCapture
    captureRoot(): RootDatabase
    capturePluginStorage?(): Database['pluginCustomStorage'] | null
    publishPluginStorageWorkingSet?(storage: Database['pluginCustomStorage']): void
    publishPluginStorageMutations?(
        mutations: readonly PluginStorageMutation[],
        keys: readonly string[],
    ): void
    capturePresetRecords?(): readonly botPreset[]
    capturePresets?(): botPreset[] | null
    captureSelectedCharacter(): CompleteCharacter | null
    captureCharacter(id: string): CompleteCharacter | null
    getSelectedCharacterId(): string | null | undefined
    getSelectedConversationId?(): string | null | undefined
    replaceDatabase(
        database: Database,
        activeCharacterIds?: ReadonlySet<string>,
        forceScalableProjection?: boolean,
    ): void
    publishPresetWorkingSet?(state: PersistentPresetMutationResult): void
    publishRootWorkingSet?(root: RootDatabase): void
    publishCharacterMutation?(state: PersistentCharacterMutationResult): void
    publishConversationReplacement?(result: PersistentConversationReplacementResult): void
    installCompleteDatabase?(database: Database): void
    restoreSelection?(characterId: string | null, conversationId: string | null): void
    publishCharacter(character: CompleteCharacter): void
    publishCharacterSet?(primary: CompleteCharacter, related: CharacterDetail[]): void
    publishConversation(
        characterId: string,
        conversation: Chat,
        nextCharacter?: CompleteCharacter,
        options?: ConversationPublicationOptions,
    ): void
    captureActivationRollback?(characterIds: readonly string[]): () => void
    shouldHydrateFullCharacter?(): boolean
    canReleaseConversation?(
        character: CompleteCharacter,
        conversationId: string,
        nextConversationId: string,
    ): boolean
    canUseWindowedSelectedConversation?(): boolean
    isConversationOperationActive?(): boolean
    subscribeConversationOperationActive?(listener: (active: boolean) => void): () => void
    /** The working set a targeted pass patches in place of a full reprojection. */
    captureWorkingSetDatabase?(): Database | null
    /** Host caches outside the working set that a remote change invalidates. */
    onPluginStorageChanged?(owner: string, key: string): void
    getGeneratingConversations?(): readonly GeneratingConversation[]
    conversationViewportRowBudget?: number
    canActivateWorkingSet?(): boolean
    canDeactivateWorkingSet?(): boolean
    canDeactivateCharacter?(id: string): boolean
    releaseInactiveCharacter?(id: string): void
}

export interface PersistentDataRuntimeDependencies {
    store: PersistentDataStore
    state: PersistentDataRuntimeStateAdapter
    officialPublisher?: OfficialRevisionPublisher | null
    getOfficialPublisher?(): OfficialRevisionPublisher | null
    clock?: SaveCoordinatorClock
    now?(): number
    onLocalRevision?(revision: DataRevision): void
    onFlushPromise?(promise: Promise<void> | null): void
    onBackgroundError?(error: unknown): void
    onLocalSaveFailure?(error: unknown | null): void
    onWorkingSetRefreshRequired?(revision: DataRevision | null): void
    onDestructiveReplacementFenceChanged?(active: boolean): void
    /** Detaches the input synchronously before any asynchronous preparation. */
    prepareDatabase(database: Database): Promise<Database>
}

export interface PersistentActivatedLibraryGuard {
    complete(): void
    abortUnchanged(): Promise<void>
}

export interface ActivatedLibraryRecoveryLifecycle {
    beforeRefresh(): Promise<number | void>
    afterRefresh(): Promise<void>
}

export interface PersistentDataRuntime {
    withPausedPersistentWrites<T>(reason: string, operation: (token: PersistentMutationToken) => Promise<T>): Promise<T>
    beginActivatedLibraryGuard(token: PersistentMutationToken): PersistentActivatedLibraryGuard
    setActivatedLibraryRecoveryLifecycle(token: PersistentMutationToken, lifecycle: ActivatedLibraryRecoveryLifecycle): void
    refreshActivatedLibraryUnderPause(token: PersistentMutationToken): Promise<CommittedApplyOutcome>
    applyLwwReceive(request: LwwStageReceive): Promise<LwwApplyResult>
    drainLwwDeferred(expectedAuthorityEpoch?: number): Promise<void>
    commitPersistentUnitIntent(reason: string, mutations: readonly PersistentUnitMutation[], conversations?: readonly ConversationMutation[], wholeMessages?: readonly WholeMessageIntent[]): Promise<void>

    readonly store: PersistentDataStore
    readonly revision: DataRevision
    getStorageAuthorityEpoch(): number
    assertPersistentMutationAllowed(expectedAuthorityEpoch?: number): void
    markCommittedWorkingSetRefreshRequired(revision: DataRevision, error: unknown): void
    readonly pendingWorkingSetRefreshRevision: DataRevision | null
    initializeActiveWorkingSet(database: Database): Promise<void>
    expirePersistentTrash(now?: number): Promise<number>
    refreshActiveWorkingSetFromStore(revision: DataRevision): Promise<CommittedApplyOutcome>
    retryCommittedWorkingSetRefresh(): Promise<CommittedApplyOutcome | null>
    runStorageOnlyMutation(
        operation: (expectedRevision: DataRevision) => Promise<DataRevision>,
    ): Promise<void>
    markPersistentDataDirty(estimatedBytes: number): void
    flushPendingData(reason: string): Promise<void>
    flushPendingDataLocally(reason: string): Promise<void>
    acknowledgeGenerationCompletion(expectedAuthorityEpoch?: number): Promise<void>
    commitCharacterAddition(
        request: CharacterAdditionRequest,
        reason: string,
    ): Promise<void>
    activateCharacter(
        id: string,
        options?: CharacterActivationOptions,
    ): Promise<boolean>
    activateConversation(id: string): Promise<boolean>
    getActiveConversationSession(): ActiveConversationSession | null
    getSelectedConversationMode(): 'complete' | 'windowed' | null
    getActiveConversationViewportSource(): ConversationViewportSource | null
    subscribeActiveConversationViewportSource(
        listener: ActiveConversationViewportSourceListener,
    ): () => void
    captureSelectedConversationTarget(): SelectedConversationTarget | null
    captureSelectedConversationAuthority(): WindowedConversationPersistenceAuthority | null
    captureWindowedMessageMutation(target: SelectedConversationTarget, absoluteIndex: number, evidence: Readonly<Message>): Promise<WindowedConversationMutationController | null>
    acquireCompleteConversation(
        reason: string,
        target?: SelectedConversationTarget | null,
    ): Promise<CompleteConversationLease>
    captureWindowedConversationMutationController(
        target: SelectedConversationTarget,
        chat: Chat,
        absoluteStartIndex: number,
    ): WindowedConversationMutationController | null
    editWindowedChatList(
        target: SelectedConversationTarget,
        edit: (character: character | groupChat) => string | null | false,
    ): WindowedChatListEditResult
    tryDemoteSelectedConversation(
        target?: SelectedConversationTarget | null,
    ): boolean
    refreshSelectedConversationAfterReplacement(
        target: SelectedConversationTarget,
        expectedSession: ActiveConversationSession,
    ): boolean
    invalidateActiveConversationSession(): void
    deactivateActiveWorkingSet(): Promise<boolean>
    reconcileActiveCharacterIds(
        database: Database,
        selectedCharacterId: string | null,
    ): ReadonlySet<string>
    getNavigationGeneration(): number
    fenceNavigation(): number
    invalidateNavigation(): void
    replacePersistentDatabase(
        database: Database,
        reason: string,
        options?: PersistentReplacementOptions,
    ): Promise<CommittedApplyOutcome>
    mutatePersistentPluginStorage(
        reason: string,
        mutations: readonly PluginStorageMutation[],
    ): Promise<void>
    mutatePersistentPresets(
        reason: string,
        mutate: PersistentPresetMutation,
    ): Promise<void>
    appendPersistentRootModule(
        reason: string,
        input: import('./saveCoordinator').PersistentRootModuleAppend,
        signal?: AbortSignal,
    ): Promise<void>
    mutateConversationBinding(
        characterId: string,
        conversationId: string,
        patch: import('./conversationBinding').ConversationBindingPatch,
        publish: (committedPatch: import('./conversationBinding').ConversationBindingPatch) => void,
    ): Promise<void>
    mutatePersistentCharacterDetail(
        characterId: string,
        reason: string,
        mutate: PersistentCharacterDetailMutation,
    ): Promise<boolean>
    deletePersistentCharacterWithGroupReferences(
        characterId: string,
        reason: string,
    ): Promise<boolean>
    replacePersistentCompleteCharacter(
        characterId: string,
        reason: string,
        mutate: PersistentCompleteCharacterMutation,
        options?: PersistentScopedReplacementOptions,
    ): Promise<boolean>
    replacePersistentConversation(
        characterId: string,
        conversationId: string,
        reason: string,
        replacement: Chat,
        options?: PersistentScopedReplacementOptions,
    ): Promise<boolean>
    upsertPersistentCompleteCharacter(
        characterId: string,
        reason: string,
        createOrMutate: PersistentCompleteCharacterUpsert,
        options?: PersistentCompleteCharacterUpsertOptions,
    ): Promise<boolean>
    readPersistentCharacterDetail(
        characterId: string,
        reason: string,
    ): Promise<CharacterDetail | null>
    readPersistentCompleteCharacter(
        characterId: string,
        reason: string,
    ): Promise<CompleteCharacter | null>
    readPersistentConversation(
        characterId: string,
        conversationId: string,
        reason: string,
    ): Promise<Chat | null>
    readPersistentConversationAt(
        characterId: string,
        orderedPosition: number,
        reason: string,
    ): Promise<Chat | null>
    readPersistentSelectedConversation(
        characterId: string,
        reason: string,
    ): Promise<PersistentSelectedConversation | null>
    capturePersistentMutationToken(
        reason: string,
        options?: { publishOfficial?: boolean },
    ): Promise<PersistentMutationToken>
    acquireDestructiveReplacementFence(
        expected: PersistentMutationToken,
    ): Promise<PersistentDestructiveReplacementFence>
    acquireCommittedWorkingSetRefreshFence(): Promise<PersistentDestructiveReplacementFence>
    materializePersistentDatabaseSnapshot(reason: string): Promise<Database>
    materializePersistentDatabaseSnapshotWithRevision(
        reason: string,
        options?: PersistentDatabaseMaterializationOptions,
    ): Promise<PersistentDatabaseSnapshot>
    releaseInactiveWorkingSet(
        canRelease?: () => boolean | Promise<boolean>,
        isCurrent?: () => boolean,
    ): Promise<boolean>
    publishCurrentOfficialRevision(): Promise<void>
    hasPendingOfficialPublication(): boolean
}

export interface PersistentDestructiveReplacementFence {
    /** Revision the working set is pinned to while the fence is held. */
    readonly revision: DataRevision
    refreshCommittedWorkingSet(
        revision: DataRevision,
        options?: PersistentCommittedWorkingSetRefreshOptions,
    ): Promise<CommittedApplyOutcome>
    release(): void
}

export interface PersistentCommittedWorkingSetRefreshOptions {
    forceScalableProjection?: boolean
    changeSet?: ReplacementChangeSet
}

function createDynamicOfficialPublisher(
    getPublisher: () => OfficialRevisionPublisher | null,
): OfficialRevisionPublisher {
    return {
        async pin(revision) {
            const publisher = getPublisher()
            if (!publisher) {
                return {
                    publish: async () => undefined,
                    dispose: async () => undefined,
                }
            }
            return publisher.pin(revision)
        },
    }
}

export function createPersistentDataRuntime(
    dependencies: PersistentDataRuntimeDependencies,
): PersistentDataRuntime {
    let workingSet: ActiveWorkingSet
    let activatedLibraryGuard: { owner: symbol; ready: boolean; lifecycle?: ActivatedLibraryRecoveryLifecycle } | null = null
    let committedWorkingSetRecovery: Promise<CommittedApplyOutcome | null> | null = null
    let pendingRefreshChangeSet: ReplacementChangeSet | null = null
    const captureChanges = (changes: Partial<ReplacementChangeSet>): ReplacementChangeSet => ({
        root: changes.root ?? false,
        presets: changes.presets ?? false,
        pluginStorage: changes.pluginStorage ?? false,
        wholeLibrary: changes.wholeLibrary ?? false,
        characterIds: [...(changes.characterIds ?? [])],
        conversations: (changes.conversations ?? []).map((target) => ({ ...target })),
    })
    const publishCommittedProjection = (
        changes: Partial<ReplacementChangeSet>,
        publish: () => void,
    ): void => {
        try {
            publish()
        } catch (error) {
            pendingRefreshChangeSet = pendingRefreshChangeSet
                ? captureChanges({ wholeLibrary: true })
                : captureChanges(changes)
            coordinator.markCommittedWorkingSetRefreshRequired(coordinator.revision, error)
        }
    }
    const coordinator = new SaveCoordinator({
        onRoutineUnitsCommitted: dependencies.state.captureWorkingSetDatabase ? async (revision, affectedKeys) => {
            await projectAppliedUnits({revision, affectedKeys: [...affectedKeys], heldKeys: [], deferredKeys: []}, undefined, true)
        } : undefined,
        canonicalCapture: dependencies.state.canonicalCapture,
        store: dependencies.store,
        captureCharacters: dependencies.state.captureCharacters,
        beforeCapture: dependencies.state.beforeCapture,
        captureRoot: dependencies.state.captureRoot,
        capturePluginStorage: dependencies.state.capturePluginStorage,
        publishPluginStorageWorkingSet: dependencies.state.publishPluginStorageWorkingSet
            ? (storage) => publishCommittedProjection({ pluginStorage: true }, () =>
                dependencies.state.publishPluginStorageWorkingSet!(storage))
            : undefined,
        publishPluginStorageMutations: dependencies.state.publishPluginStorageMutations
            ? (mutations, keys) => publishCommittedProjection({ pluginStorage: true }, () =>
                dependencies.state.publishPluginStorageMutations!(mutations, keys))
            : undefined,
        capturePresets: dependencies.state.capturePresets,
        capturePresetRecords: dependencies.state.capturePresetRecords,
        captureSelectedCharacter: dependencies.state.captureSelectedCharacter,
        captureSelectedConversationAuthority: () =>
            workingSet.captureSelectedConversationAuthority(),
        captureCharacter: dependencies.state.captureCharacter,
        replaceDatabase: (database) => {
            workingSet.invalidateActiveConversationSession()
            const activeCharacterIds = workingSet.reconcileActiveCharacterIds(
                database,
                dependencies.state.getSelectedCharacterId() ?? null,
            )
            dependencies.state.replaceDatabase(database, activeCharacterIds)
        },
        publishPresetWorkingSet: dependencies.state.publishPresetWorkingSet
            ? (result) => publishCommittedProjection({ root: true, presets: true }, () =>
                dependencies.state.publishPresetWorkingSet!(result))
            : undefined,
        publishRootWorkingSet: dependencies.state.publishRootWorkingSet
            ? (root) => publishCommittedProjection({ root: true }, () =>
                dependencies.state.publishRootWorkingSet!(root))
            : undefined,
        publishCharacterMutation: dependencies.state.publishCharacterMutation
            ? (result) => publishCommittedProjection({
                root: true,
                characterIds: [result.characterId, ...(result.relatedCharacters?.map((value) => value.chaId) ?? [])],
            }, () => dependencies.state.publishCharacterMutation!(result))
            : undefined,
        publishConversationReplacement: dependencies.state.publishConversationReplacement
            ? (result) => publishCommittedProjection({
                conversations: [{ characterId: result.characterId, conversationId: result.conversationId }],
            }, () => dependencies.state.publishConversationReplacement!(result))
            : undefined,
        isIncompleteWorkingSet: (database) =>
            hasIncompletePersistentWorkingSet(database, workingSetResidency),
        getNavigationGeneration: () => workingSet.navigationGenerationToken,
        officialPublisher: dependencies.officialPublisher || dependencies.getOfficialPublisher
            ? createDynamicOfficialPublisher(
                dependencies.getOfficialPublisher
                    ?? (() => dependencies.officialPublisher ?? null),
            )
            : undefined,
        clock: dependencies.clock,
        now: dependencies.now,
        onLocalRevision: (revision) => publishCommittedProjection({ wholeLibrary: true }, () => {
            workingSet.advanceStoreRevision(revision)
            dependencies.onLocalRevision?.(revision)
        }),
        onStorageOnlyRevision: (revision) => workingSet.advanceStoreRevision(revision),
        onWindowedSelectedConversationRevision: (revision) =>
            workingSet.advanceStoreRevision(revision),
        onConversationMutationPersistenceStarted: (event) =>
            workingSet.beginConversationMutationPersistence(event),
        onConversationMutationPersisted: (event) => {
            workingSet.acknowledgeConversationMutationPersisted(event)
        },
        onConversationMutationFallbackPersisted: (event) => {
            workingSet.acknowledgeConversationMutationFallbackPersisted(event)
        },
        onPersistenceIdle: () => workingSet.scheduleSelectedConversationDemotion(),
        onFlushPromise: dependencies.onFlushPromise,
        onBackgroundError: dependencies.onBackgroundError,
        onLocalSaveFailure: dependencies.onLocalSaveFailure,
        onWorkingSetRefreshRequired: (revision) => {
            if (revision === null) pendingRefreshChangeSet = null
            dependencies.onWorkingSetRefreshRequired?.(revision)
        },
        onDestructiveReplacementFenceChanged: (active) => {
            // A projection can request demotion while its input guard still blocks it.
            // Retry only after the final release, preserving all ordinary demotion checks.
            if (!active) {
                workingSet.scheduleSelectedConversationDemotion()
                queueMicrotask(() => retryDeferredContent())
            }
            dependencies.onDestructiveReplacementFenceChanged?.(active)
        },
        isConversationOperationActive: dependencies.state.isConversationOperationActive,
    })
    workingSet = new ActiveWorkingSet({
        store: dependencies.store,
        coordinator,
        getSelectedCharacterId: dependencies.state.getSelectedCharacterId,
        getResidentCharacter: dependencies.state.captureCharacter,
        publishCharacter: dependencies.state.publishCharacter,
        publishCharacterSet: dependencies.state.publishCharacterSet ?? ((primary, related) => {
            for (const detail of related) {
                const resident = dependencies.state.captureCharacter(detail.chaId)
                if (resident) dependencies.state.publishCharacter({
                    ...detail,
                    chats: resident.chats,
                } as CompleteCharacter)
            }
            dependencies.state.publishCharacter(primary)
        }),
        publishConversation: dependencies.state.publishConversation,
        captureActivationRollback: dependencies.state.captureActivationRollback,
        canActivateWorkingSet: () =>
            coordinator.pendingWorkingSetRefreshRevision === null &&
            dependencies.state.canActivateWorkingSet?.() !== false,
        canDeactivateWorkingSet: () =>
            coordinator.pendingWorkingSetRefreshRevision === null &&
            dependencies.state.canDeactivateWorkingSet?.() !== false,
        canDeactivateCharacter: dependencies.state.canDeactivateCharacter,
        releaseInactiveCharacter: dependencies.state.releaseInactiveCharacter,
        shouldHydrateFullCharacter: dependencies.state.shouldHydrateFullCharacter,
        canReleaseConversation: dependencies.state.canReleaseConversation,
        canUseWindowedSelectedConversation:
            dependencies.state.canUseWindowedSelectedConversation,
        isConversationOperationActive: dependencies.state.isConversationOperationActive,
        subscribeConversationOperationActive:
            dependencies.state.subscribeConversationOperationActive,
        conversationViewportRowBudget: dependencies.state.conversationViewportRowBudget,
    })
    const activateCharacter = (
        id: string,
        options?: CharacterActivationOptions,
    ): Promise<boolean> => {
        coordinator.assertPersistentMutationAllowed()
        const prepare = options?.prepare
        const normalize = options?.normalize
        if (!prepare) {
            return workingSet.activateCharacter(
                id,
                normalize ? { normalize } : undefined,
            )
        }
        return workingSet.activateCharacter(id, {
            normalize,
            async prepare() {
                const prepared = await prepare()
                if (!prepared) return null
                return {
                    ...prepared,
                    database: await dependencies.prepareDatabase(prepared.database),
                }
            },
        })
    }
    let deferredContentPending = false
    const commitContentCursor = async (revision: DataRevision, strict = false): Promise<void> => {
        const commit = dependencies.store.commitWorkingSetChangeCursor
        if (!commit) return
        try {
            await commit.call(dependencies.store, revision)
        } catch (error) {
            if (strict) throw error
            // The projection is installed either way; a stale cursor only costs
            // the next window an idempotent replay.
            dependencies.onBackgroundError?.(error)
        }
    }
    /// The change window and every record reprojected for it are read through
    /// one lease, so a targeted pass sees exactly the content it explains.
    const projectRefreshedWorkingSet = async (
        revision: DataRevision,
        pinned: {
            selectedCharacterId: string | null
            selectedConversationId: string | null
            activeCharacterIds: ReadonlySet<string>
        },
        changeSet?: ReplacementChangeSet,
    ): Promise<{ database: Database; deferred: boolean; windowedMetadata?: PersistentConversationMetadata }> => {
        const lease = await dependencies.store.acquireRevision(revision)
        let primaryError: unknown
        try {
            assertPinnedRevision(revision, lease.revision, 'Revision lease')
            const previous = dependencies.state.captureWorkingSetDatabase?.() ?? null
            const authority = workingSet.captureSelectedConversationAuthority()
            let windowedMetadata: PersistentConversationMetadata | undefined
            if (authority?.kind === 'windowed' &&
                authority.sessionVersion === authority.persistedSessionVersion &&
                pinned.selectedCharacterId === authority.characterId &&
                pinned.selectedConversationId === authority.conversationId) {
                const metadata = await lease.readConversationMetadata(authority.characterId, authority.conversationId)
                if (metadata) {
                    assertPinnedRevision(revision, metadata.revision, 'Selected conversation metadata')
                    windowedMetadata = metadata.value
                }
            }
            const shell = windowedMetadata && createMetadataOnlySelectedConversation(windowedMetadata.conversation)
            const keepConversation = shell ? (id: string) => id === shell.id ? shell : null : undefined
            if (previous && isProjectedWorkingSet(previous)) {
                try {
                    const keys = changeSet
                        ? (changeSet.wholeLibrary ? null : [])
                        : await readWorkingSetChangeWindow(lease)
                    if (keys) {
                        // Only the selected conversation can be held by a targeted pass.
                        const deferredConversation =
                            dependencies.state.isConversationOperationActive?.() === true
                                ? generating().find((target) =>
                                    target.characterId === pinned.selectedCharacterId &&
                                    target.conversationId === pinned.selectedConversationId) ?? null
                                : null
                        const targeted = await applyTargetedWorkingSetInvalidation(
                            previous,
                            keys,
                            lease,
                            {
                                ...pinned,
                                keepConversation,
                                changeSet,
                                deferredConversation,
                                onPluginStorageChanged:
                                    dependencies.state.onPluginStorageChanged,
                            },
                        )
                        if (targeted) return { ...targeted, windowedMetadata }
                    }
                } catch (error) {
                    // A failed pass leaves the cursor where it was and the whole
                    // working set is reprojected instead.
                    dependencies.onBackgroundError?.(error)
                }
            }
            return {
                database: await projectPinnedScalableWorkingSet(lease, { ...pinned, keepConversation }),
                deferred: false,
                windowedMetadata,
            }
        } catch (error) {
            primaryError = error
            throw error
        } finally {
            try {
                await releasePersistentRevisionLease(lease)
            } catch (error) {
                if (primaryError === undefined) throw error
            }
        }
    }
    const requireCommittedRefresh = (
        revision: DataRevision,
        error: unknown,
    ): CommittedApplyOutcome => {
        coordinator.markCommittedWorkingSetRefreshRequired(revision, error)
        return {
            kind: 'committed',
            revision: coordinator.revision,
            projection: 'refresh-required',
        }
    }
    const refreshCommittedWorkingSet = async (
        revision: DataRevision,
        fenceOwner: symbol,
        options?: PersistentCommittedWorkingSetRefreshOptions,
    ): Promise<CommittedApplyOutcome> => {
        try {
            coordinator.assertDestructiveReplacementFence(fenceOwner)
            const navigationGeneration = workingSet.navigationGenerationToken
            const selectedCharacterId =
                dependencies.state.getSelectedCharacterId() ?? null
            const selectedConversationId =
                dependencies.state.getSelectedConversationId?.() ?? null
            const activeCharacterIds = activatedLibraryGuard ? new Set<string>() : workingSet.activeCharacterIds
            const projected = await projectRefreshedWorkingSet(revision, {
                selectedCharacterId: activatedLibraryGuard ? null : selectedCharacterId,
                selectedConversationId: activatedLibraryGuard ? null : selectedConversationId,
                activeCharacterIds,
            }, activatedLibraryGuard ? captureChanges({wholeLibrary: true}) : options?.changeSet)
            if (activatedLibraryGuard && projected.deferred) {
                throw new Error('Activated library projection is incomplete')
            }
            coordinator.assertDestructiveReplacementFence(fenceOwner)
            if (
                navigationGeneration !== workingSet.navigationGenerationToken ||
                selectedCharacterId !== (dependencies.state.getSelectedCharacterId() ?? null) ||
                selectedConversationId !== (dependencies.state.getSelectedConversationId?.() ?? null)
            ) {
                throw new PersistentMutationFencedError()
            }
            workingSet.invalidateNavigation()
            dependencies.state.replaceDatabase(
                projected.database,
                activeCharacterIds,
                options?.forceScalableProjection ?? true,
            )
            if (activatedLibraryGuard) dependencies.state.afterRemoteApply?.()
            workingSet.installCommittedWorkingSet(projected.database, revision, projected.windowedMetadata)
            deferredContentPending = projected.deferred
            if (!projected.deferred) await commitContentCursor(revision, activatedLibraryGuard !== null)
            if (activatedLibraryGuard) {
                await activatedLibraryGuard.lifecycle?.afterRefresh()
                coordinator.finishActivatedLibraryGuard(activatedLibraryGuard.owner)
                activatedLibraryGuard = null
            }
            return { kind: 'committed', revision, projection: 'applied' }
        } catch (error) {
            return requireCommittedRefresh(revision, error)
        }
    }
    const acquireCommittedWorkingSetRefreshFence =
        async (): Promise<PersistentDestructiveReplacementFence> => {
            const owner =
                await coordinator.acquireCommittedWorkingSetRefreshFence()
            const heldRevision = coordinator.revision
            let released = false
            return {
                revision: heldRevision,
                async refreshCommittedWorkingSet(minimumRevision, options) {
                    options = options && {
                        ...options,
                        changeSet: options.changeSet ? captureChanges(options.changeSet) : undefined,
                    }
                    if (released) {
                        throw new Error(
                            'Destructive persistent replacement fence was released',
                        )
                    }
                    try {
                        coordinator.assertDestructiveReplacementFence(owner)
                        const reconciledRevision = await activatedLibraryGuard?.lifecycle?.beforeRefresh()
                        coordinator.assertDestructiveReplacementFence(owner)
                        const latest = await dependencies.store.readRoot()
                        coordinator.assertDestructiveReplacementFence(owner)
                        const minimum = Math.max(
                            minimumRevision,
                            coordinator.pendingWorkingSetRefreshRevision ?? minimumRevision,
                            typeof reconciledRevision === 'number' ? reconciledRevision : minimumRevision,
                        )
                        if (latest.revision < minimum) {
                            throw new RevisionConflictError(minimum, latest.revision)
                        }
                        return refreshCommittedWorkingSet(latest.revision, owner, {
                            ...options,
                            changeSet: latest.revision === minimumRevision ? options?.changeSet : undefined,
                        })
                    } catch (error) {
                        return requireCommittedRefresh(minimumRevision, error)
                    }
                },
                release() {
                    if (released) return
                    coordinator.releaseDestructiveReplacementFence(owner)
                    released = true
                },
            }
        }
    // A change held back from a generating conversation is applied once that
    // generation ends; until then the cursor stays behind it. The generated
    // reply is persisted first, so reprojecting cannot discard it.
    let deferredRefreshRunning = false
    const retryDeferredContent = () => {
        if (deferredRefreshRunning || !deferredContentPending ||
            dependencies.state.isConversationOperationActive?.() || coordinator.hasDestructiveReplacementFence) return
        deferredRefreshRunning = true
        void (async () => {
            try {
                await coordinator.flushPendingDataLocally('deferred-content-change')
                const token = await coordinator.capturePersistentMutationToken(
                    'deferred-content-change', { publishOfficial: false },
                )
                const owner = await coordinator.acquireDestructiveReplacementFence(token)
                try {
                    await refreshCommittedWorkingSet(token.revision, owner)
                } finally {
                    coordinator.releaseDestructiveReplacementFence(owner)
                }
            } catch (error) {
                if (!(error instanceof PersistentMutationFencedError) && !(error instanceof RevisionConflictError)) {
                    dependencies.onBackgroundError?.(error)
                }
            } finally {
                deferredRefreshRunning = false
            }
        })()
    }
    dependencies.state.subscribeConversationOperationActive?.((active) => {
        if (!active) retryDeferredContent()
    })
    const withCompleteCharacter = async <T>(characterId: string, operation: () => Promise<T>): Promise<T> => {
        const target = workingSet.captureSelectedConversationTarget()
        const lease = target?.characterId === characterId && workingSet.selectedConversationMode === 'windowed'
            ? await workingSet.acquireCompleteConversation('explicit-mutation', target)
            : null
        try {
            return await operation()
        } finally {
            lease?.release()
        }
    }
    const generating = () => [...(dependencies.state.getGeneratingConversations?.() ?? generatingConversations.snapshot())]
    const projectAppliedUnits = async (result: LwwApplyResult, baseline?: ReturnType<typeof captureLwwWorkingSetBaseline>, localIntent = false): Promise<void> => {
        const database = dependencies.state.captureWorkingSetDatabase?.()
        if (!database) { coordinator.adoptAppliedUnitState(result.revision, null, null, []); return }
        baseline ??= captureLwwWorkingSetBaseline(database, coordinator.capturePersistentBaselineRoot(), dependencies.state.capturePresets?.() ?? null, coordinator.captureMaterializedBaseline(), coordinator.capturePresetRecordBaseline())
        const selectedTarget = workingSet.captureSelectedConversationTarget()
        const selectedSession = workingSet.activeConversationSession
        // Remote values are patched into the live conversation in place; the
        // session that renders it is told through its own version instead.
        const selectedOwner = selectedTarget ? database.characters.find((value) => value.chaId === selectedTarget.characterId) : undefined
        const selectedChat = selectedOwner?.chats.find((value) => value.id === selectedTarget!.conversationId)
        const selectedFields = selectedChat && selectedSession?.matchesConversation(selectedTarget!.characterId, selectedChat)
            ? new Map(result.affectedKeys.map((key) => JSON.parse(key) as string[])
                .filter(([kind, characterId, conversationId]) => (kind === 'conversation' || kind === 'messages') && characterId === selectedTarget!.characterId && conversationId === selectedTarget!.conversationId)
                .map(([kind, , , field]) => kind === 'messages' ? 'message' : field)
                .map((field) => [field, (selectedChat as unknown as Record<string, unknown>)[field]] as const))
            : undefined
        const authorityEpoch = coordinator.storageAuthorityEpoch
        const lease = await dependencies.store.acquireRevision(result.revision)
        const projectionBaseline = baseline
        let applied: ReturnType<typeof captureLwwWorkingSetBaseline>
        try {
            applied = await applyLwwWorkingSetUnits(database, projectionBaseline, lease, result.affectedKeys, dependencies.state.captureCharacterIndex?.(), localIntent, () => {
                coordinator.assertPersistentMutationAllowed(authorityEpoch)
                if (dependencies.state.captureWorkingSetDatabase?.() !== database) throw new Error('Persistent working set changed during unit projection')
                dependencies.state.beforeCapture?.()
            }, () => {
                const canonicalCapture = dependencies.state.canonicalCapture
                const beforeDerive = canonicalCapture?.root() ?? canonicalJson(dependencies.state.captureRoot())
                dependencies.state.afterRemoteApply?.()
                const afterDerive = canonicalCapture?.root() ?? canonicalJson(dependencies.state.captureRoot())
                const derivedMutations = beforeDerive === afterDerive ? [] : canonicalCapture
                    ? canonicalCapture.diffRoot(beforeDerive, afterDerive)
                    : diffRootMutations(JSON.parse(beforeDerive), JSON.parse(afterDerive))
                for (const mutation of derivedMutations) {
                    const root = projectionBaseline.root as unknown as Record<string, unknown>
                    if (mutation.type === 'set') root[mutation.key] = canonicalClone(mutation.value)
                    else delete root[mutation.key]
                }
            })
        } finally { await releasePersistentRevisionLease(lease) }
        coordinator.adoptAppliedUnitState(result.revision, applied.root, applied.presets, applied.characters, applied.presetRecords)
        if (selectedFields && [...selectedFields].some(([field, value]) => (selectedChat as unknown as Record<string, unknown>)[field] !== value) &&
            selectedSession!.matchesConversation(selectedTarget!.characterId, selectedChat!) &&
            !selectedSession!.adoptPersistedMetadata(selectedChat!, result.revision)) {
            // A session that cannot adopt the change is republished from a new object.
            const index = selectedOwner!.chats.indexOf(selectedChat!)
            if (index >= 0) selectedOwner!.chats[index] = { ...selectedChat! }
        }
        if (selectedTarget && selectedSession) workingSet.refreshSelectedConversationAfterReplacement(selectedTarget, selectedSession)
        for (const key of result.affectedKeys) {
            const [kind, owner, name] = JSON.parse(key)
            if (kind === 'plugin') dependencies.state.onPluginStorageChanged?.(owner, name)
            else if (kind === 'order' && owner === 'plugin-storage') {
                dependencies.state.onPluginStorageChanged?.(name, '')
            }
        }
        await commitContentCursor(result.revision)
    }
    const drainLwwDeferred = async (authorityEpoch = coordinator.storageAuthorityEpoch): Promise<void> => {
        coordinator.assertPersistentMutationAllowed(authorityEpoch)
        if (!dependencies.store.lwwBindingState || !dependencies.store.lwwDrainDeferred) return
        await coordinator.withPausedPersistentWrites('generation-deferred-drain', async () => {
            coordinator.assertPersistentMutationAllowed(authorityEpoch)
            const binding = await dependencies.store.lwwBindingState!()
            coordinator.assertPersistentMutationAllowed(authorityEpoch)
            const database = dependencies.state.captureWorkingSetDatabase?.()
            const baseline = database ? captureLwwWorkingSetBaseline(database, dependencies.state.captureRoot(), dependencies.state.capturePresets?.() ?? null, coordinator.captureMaterializedBaseline(), coordinator.capturePresetRecordBaseline()) : undefined
            const result = await dependencies.store.lwwDrainDeferred!({ bindingAuthority: binding.targetAuthority, requestId: crypto.randomUUID(), generating: generating() })
            try {
                coordinator.assertPersistentMutationAllowed(authorityEpoch)
                await projectAppliedUnits(result, baseline)
                coordinator.assertPersistentMutationAllowed(authorityEpoch)
            } catch (error) {
                coordinator.markCommittedWorkingSetRefreshRequired(result.revision, error)
                throw error
            }
        })
    }
    const runtime: PersistentDataRuntime = {
        store: dependencies.store,
        withPausedPersistentWrites: (reason, operation) => coordinator.withPausedPersistentWrites(reason, operation),
        setActivatedLibraryRecoveryLifecycle(token, lifecycle) {
            const guard = activatedLibraryGuard
            if (!guard || guard.lifecycle) throw new PersistentMutationFencedError()
            coordinator.assertActivatedLibraryGuard(guard.owner, token)
            guard.lifecycle = lifecycle
        },
        beginActivatedLibraryGuard(token) {
            const owner = coordinator.beginActivatedLibraryGuard(token)
            const guard = {owner, ready: false}
            activatedLibraryGuard = guard
            return {
                complete() {
                    if (activatedLibraryGuard !== guard || !guard.ready) throw new PersistentMutationFencedError()
                    coordinator.assertActivatedLibraryGuard(owner, token, false)
                    coordinator.finishActivatedLibraryGuard(owner)
                    activatedLibraryGuard = null
                },
                async abortUnchanged() {
                    if (activatedLibraryGuard !== guard || guard.ready) throw new PersistentMutationFencedError()
                    const latest = await dependencies.store.readRoot()
                    if (latest.revision !== token.revision || coordinator.revision !== token.revision) {
                        throw new PersistentMutationFencedError()
                    }
                    coordinator.finishActivatedLibraryGuard(owner)
                    activatedLibraryGuard = null
                },
            }
        },
        async refreshActivatedLibraryUnderPause(token) {
            const guard = activatedLibraryGuard
            if (!guard) throw new PersistentMutationFencedError()
            coordinator.assertActivatedLibraryGuard(guard.owner, token)
            const latest = await dependencies.store.readRoot()
            const projected = await projectRefreshedWorkingSet(latest.revision, {selectedCharacterId:null, selectedConversationId:null, activeCharacterIds:new Set()}, {wholeLibrary:true, root:true, presets:true, pluginStorage:true, characterIds:[], conversations:[]})
            if (projected.deferred) throw new Error('Activated library projection is incomplete')
            coordinator.assertActivatedLibraryGuard(guard.owner, token)
            workingSet.invalidateNavigation()
            dependencies.state.replaceDatabase(projected.database, new Set(), true)
            dependencies.state.afterRemoteApply?.()
            workingSet.installCommittedWorkingSet(projected.database, latest.revision, projected.windowedMetadata)
            await commitContentCursor(latest.revision, true)
            guard.ready = true
            return {kind:'committed', revision:latest.revision, projection:'applied'}
        },
        async applyLwwReceive(request) {
            const staged = canonicalClone(request)
            return coordinator.withPausedPersistentWrites('lww-receive', async () => {
                const store = dependencies.store
                if (!store.lwwStageReceive || !store.lwwApplyReceive || !store.lwwFinishReceive) throw new Error('LWW receive is unavailable')
                const database = dependencies.state.captureWorkingSetDatabase?.()
                const baseline = database ? captureLwwWorkingSetBaseline(database, dependencies.state.captureRoot(), dependencies.state.capturePresets?.() ?? null, coordinator.captureMaterializedBaseline(), coordinator.capturePresetRecordBaseline()) : undefined
                await store.lwwStageReceive(staged)
                const header = { bindingAuthority: staged.bindingAuthority, requestId: staged.requestId }
                const result = await store.lwwApplyReceive({ ...header, generating: generating() })
                try { await projectAppliedUnits(result, baseline) }
                catch (error) { coordinator.markCommittedWorkingSetRefreshRequired(result.revision, error); throw error }
                finally { await store.lwwFinishReceive(header) }
                return result
            })
        },
        drainLwwDeferred,
        async commitPersistentUnitIntent(reason, mutations, conversations, wholeMessages) {
            // Translation reads the effective identity records, so unsaved mirror
            // edits reach them first.
            dependencies.state.beforeCapture?.()
            const translated = translatePersistentRootUnitIntents(mutations)
            const affectedKeys = translated.map((value) => value.key)
            for (const value of wholeMessages ?? []) affectedKeys.push(JSON.stringify(['messages', value.characterId, value.conversationId]))
            for (const value of conversations ?? []) {
                if (value.type === 'delete') affectedKeys.push(JSON.stringify(['exists', 'conversation', value.characterId, value.conversationId]))
                else if (value.type === 'reorder') affectedKeys.push(JSON.stringify(['order', 'conversations', value.characterId]))
                else affectedKeys.push(JSON.stringify(['messages', value.characterId, value.conversationId]))
            }
            await coordinator.commitPersistentUnitIntent(reason, translated, conversations, wholeMessages, async (revision) => {
                try { await projectAppliedUnits({ revision, affectedKeys, heldKeys: [], deferredKeys: [] }, undefined, true) }
                catch (error) { coordinator.markCommittedWorkingSetRefreshRequired(revision, error); throw error }
            })
        },
        get revision() {
            return coordinator.revision
        },
        getStorageAuthorityEpoch: () => coordinator.storageAuthorityEpoch,
        assertPersistentMutationAllowed: (expectedAuthorityEpoch) =>
            coordinator.assertPersistentMutationAllowed(expectedAuthorityEpoch),
        markCommittedWorkingSetRefreshRequired: (revision, error) =>
            coordinator.markCommittedWorkingSetRefreshRequired(revision, error),
        get pendingWorkingSetRefreshRevision() {
            return coordinator.pendingWorkingSetRefreshRevision
        },
        async initializeActiveWorkingSet(database) {
            if (activatedLibraryGuard) throw new PersistentMutationFencedError()
            dependencies.state.afterRemoteApply?.()
            const result = await workingSet.initializeActiveWorkingSet(database)
            await drainLwwDeferred()
            // A recreated WebView starts from a projection of the current
            // revision, so the cursor is realigned with it.
            await commitContentCursor(coordinator.revision)
            return result
        },
        async refreshActiveWorkingSetFromStore(revision) {
            const fence = await acquireCommittedWorkingSetRefreshFence()
            try {
                return await fence.refreshCommittedWorkingSet(revision)
            } finally {
                fence.release()
            }
        },
        retryCommittedWorkingSetRefresh() {
            if (committedWorkingSetRecovery) return committedWorkingSetRecovery
            const recovering = (async () => {
                const revision = coordinator.pendingWorkingSetRefreshRevision
                if (revision === null) return null
                const changes = pendingRefreshChangeSet
                const epoch = coordinator.storageAuthorityEpoch
                const guardedRecovery = activatedLibraryGuard !== null || hasRetryableCommittedWorkingSetContinuation(runtime, epoch)
                let adoptedEpoch = epoch
                let ownsRefreshFence = false
                const releaseRecovery = claimCommittedWorkingSetRecovery(runtime, epoch,
                    activatedLibraryGuard?.owner ?? Symbol('critical-working-set-recovery'),
                    () => ownsRefreshFence || coordinator.storageAuthorityEpoch === adoptedEpoch)
                try {
                    const fence = await acquireCommittedWorkingSetRefreshFence()
                    ownsRefreshFence = true
                    let outcome: CommittedApplyOutcome
                    try {
                        outcome = await fence.refreshCommittedWorkingSet(revision, {changeSet: changes ?? undefined})
                    } finally {
                        adoptedEpoch = coordinator.storageAuthorityEpoch
                        ownsRefreshFence = false
                        fence.release()
                    }
                    if (guardedRecovery) rebaseCommittedWorkingSetContinuation(runtime, epoch, adoptedEpoch)
                    if (guardedRecovery && outcome.projection === 'applied') {
                        try { await continueCommittedWorkingSetRefresh(outcome.revision, runtime, adoptedEpoch) }
                        catch (error) {
                            if (coordinator.storageAuthorityEpoch !== adoptedEpoch) throw error
                            return requireCommittedRefresh(outcome.revision, error)
                        }
                    }
                    return outcome
                } finally { releaseRecovery() }
            })()
            committedWorkingSetRecovery = recovering
            const clearRecovery = () => { if (committedWorkingSetRecovery === recovering) committedWorkingSetRecovery = null }
            void recovering.then(clearRecovery, clearRecovery)
            return recovering
        },
        runStorageOnlyMutation: (operation) =>
            coordinator.runStorageOnlyMutation(operation),
        markPersistentDataDirty: (estimatedBytes) =>
            coordinator.markPersistentDataDirty(estimatedBytes),
        flushPendingData: (reason) => coordinator.flushPendingData(reason),
        flushPendingDataLocally: (reason) =>
            coordinator.flushPendingDataLocally(reason),
        async acknowledgeGenerationCompletion(expectedAuthorityEpoch = coordinator.storageAuthorityEpoch) {
            coordinator.assertPersistentMutationAllowed(expectedAuthorityEpoch)
            await coordinator.flushPendingDataLocally('generation-completion')
            coordinator.assertPersistentMutationAllowed(expectedAuthorityEpoch)
            await drainLwwDeferred(expectedAuthorityEpoch)
        },
        commitCharacterAddition: (request, reason) =>
            coordinator.commitCharacterAddition(request, reason),
        activateCharacter,
        activateConversation: (id) => workingSet.activateConversation(id),
        getActiveConversationSession: () =>
            workingSet.activeConversationSession,
        getSelectedConversationMode: () => workingSet.selectedConversationMode,
        getActiveConversationViewportSource: () =>
            workingSet.activeConversationViewportSource,
        subscribeActiveConversationViewportSource: (listener) =>
            workingSet.subscribeActiveConversationViewportSource(listener),
        captureSelectedConversationTarget: () =>
            workingSet.captureSelectedConversationTarget(),
        captureSelectedConversationAuthority: () =>
            workingSet.captureSelectedConversationAuthority(),
        acquireCompleteConversation: (reason, target) =>
            workingSet.acquireCompleteConversation(reason, target ?? undefined),
        captureWindowedMessageMutation: (target, absoluteIndex, evidence) => workingSet.captureWindowedMessageMutation(target, absoluteIndex, evidence),
        captureWindowedConversationMutationController: (target, chat, absoluteStartIndex) =>
            workingSet.captureWindowedConversationMutationController(
                target,
                chat,
                absoluteStartIndex,
            ),
        editWindowedChatList: (target, edit) =>
            workingSet.editWindowedChatList(target, edit),
        tryDemoteSelectedConversation: (target) =>
            workingSet.tryDemoteSelectedConversation(target ?? undefined),
        refreshSelectedConversationAfterReplacement: (
            target,
            expectedSession,
        ) => coordinator.pendingWorkingSetRefreshRevision === null &&
            workingSet.refreshSelectedConversationAfterReplacement(
                target,
                expectedSession,
            ),
        invalidateActiveConversationSession: () =>
            workingSet.invalidateActiveConversationSession(),
        deactivateActiveWorkingSet: () => workingSet.deactivate(),
        reconcileActiveCharacterIds: (database, selectedCharacterId) =>
            workingSet.reconcileActiveCharacterIds(
                database,
                selectedCharacterId,
            ),
        getNavigationGeneration: () => workingSet.navigationGenerationToken,
        fenceNavigation: () => workingSet.fenceNavigation(),
        invalidateNavigation: () => workingSet.invalidateNavigation(),
        replacePersistentDatabase: async (database, reason, options) => {
            if (
                !options?.authoritative &&
                hasIncompletePersistentWorkingSet(database, workingSetResidency)
            ) {
                return Promise.reject(
                    new Error(
                        'Cannot replace persistent data from an incomplete persistent working set',
                    ),
                )
            }
            if (isTauri && options?.upstreamImport) {
                const {prepareUpstreamImport} = await import('./importedIdentity')
                const candidate = await dependencies.prepareDatabase(prepareUpstreamImport(database))
                const {prepareBoundLibraryReplacement} = await import('./sync/bindingRegistry')
                const binding = await prepareBoundLibraryReplacement()
                const {acquireUpstreamImportPause, confirmUpstreamLibraryReplacement} = await import('./upstreamReplacement')
                if (!(await confirmUpstreamLibraryReplacement(binding.bound, options.upstreamImportWarnings))) throw new DOMException('Import cancelled', 'AbortError')
                const plugins = await import('../plugins/apiV3/v3.svelte')
                let pause: Awaited<ReturnType<typeof acquireUpstreamImportPause>>
                try {
                    await binding.fence()
                    await plugins.fencePluginExecutionForAuthorityReplacement()
                    pause = await acquireUpstreamImportPause(runtime, reason)
                } catch (error) {
                    try {
                        await binding.assertAuthority()
                        await plugins.restartPluginsAfterAuthorityReplacement()
                        await binding.resume()
                    } catch {}
                    throw error
                }
                let committed = false
                let acceptedRevision = pause.fence.revision
                const restartPlugins = async () => {
                    await plugins.invalidatePluginCachesAfterAuthorityReplacement()
                    await plugins.restartPluginsAfterAuthorityReplacement()
                    options.onPluginsRestarted?.()
                }
                runtime.setActivatedLibraryRecoveryLifecycle(pause.token, {
                    beforeRefresh: () => binding.assertAuthority(),
                    afterRefresh: restartPlugins,
                })
                try {
                    if ((options.expectedRevision !== undefined && options.expectedRevision !== pause.fence.revision) ||
                        (options.expectedMutationGeneration !== undefined && options.expectedMutationGeneration !== pause.token.mutationGeneration)) {
                        throw new PersistentMutationFencedError()
                    }
                    await binding?.assertAuthority()
                    const result = await dependencies.store.replaceFromDatabase(candidate, pause.fence.revision, [], options.pluginStorageValues,
                        binding.bound ? {bindingAuthority:binding.state.targetAuthority,requestId:crypto.randomUUID()} : undefined)
                    committed = true
                    acceptedRevision = result.revision
                    const outcome = await pause.fence.refreshCommittedWorkingSet(result.revision)
                    await restartPlugins()
                    pause.complete()
                    await pause.finish()
                    await coordinator.finishUpstreamReplacementPublication(outcome.revision, options.publishOfficial)
                    await binding?.resume()
                    return outcome
                } catch (error) {
                    let abortedUnchanged = false
                    if (!committed) {
                        try {
                            await pause.abortUnchanged(async () => { await binding.assertAuthority() })
                            abortedUnchanged = true
                            await plugins.restartPluginsAfterAuthorityReplacement()
                            await binding.resume()
                        } catch { await pause.finish() }
                    } else await pause.finish()
                    if (!abortedUnchanged) {
                        coordinator.markCommittedWorkingSetRefreshRequired(acceptedRevision, error)
                        registerCommittedWorkingSetContinuation(acceptedRevision, runtime, runtime.getStorageAuthorityEpoch(), async () => {
                            await binding.assertAuthority()
                            await coordinator.finishUpstreamReplacementPublication(coordinator.revision, options.publishOfficial)
                            await binding.resume()
                        }, undefined, true)
                    }
                    throw error
                }
            }
            return coordinator.replacePreparedPersistentDatabase(
                () => dependencies.prepareDatabase(database),
                reason,
                options,
            )
        },
        mutatePersistentPluginStorage: (reason, mutations) =>
            coordinator.mutatePersistentPluginStorage(reason, mutations),
        mutatePersistentPresets: (reason, mutate) =>
            coordinator.mutatePersistentPresets(reason, mutate),
        appendPersistentRootModule: (reason, input, signal) =>
            coordinator.appendPersistentRootModule(reason, input, signal),
        mutateConversationBinding: (
            characterId,
            conversationId,
            patch,
            publish,
        ) =>
            coordinator.mutateConversationBinding(
                characterId,
                conversationId,
                patch,
                publish,
            ),
        expirePersistentTrash: (now) => {
            const selected = dependencies.state.captureSelectedCharacter()
            return withCompleteCharacter(selected?.type === 'group' ? selected.chaId : '', () =>
                coordinator.expirePersistentTrash(now))
        },
        mutatePersistentCharacterDetail: (characterId, reason, mutate) =>
            withCompleteCharacter(characterId, () => coordinator.mutatePersistentCharacterDetail(
                characterId,
                reason,
                mutate,
            )),
        deletePersistentCharacterWithGroupReferences: (characterId, reason) =>
            withCompleteCharacter(dependencies.state.captureSelectedCharacter()?.type === 'group'
                ? dependencies.state.captureSelectedCharacter()!.chaId : characterId, () => coordinator.deletePersistentCharacterWithGroupReferences(
                characterId,
                reason,
            )),
        replacePersistentCompleteCharacter: (
            characterId,
            reason,
            mutate,
            options,
        ) =>
            withCompleteCharacter(characterId, () => coordinator.replacePersistentCompleteCharacter(
                characterId,
                reason,
                mutate,
                options,
            )),
        replacePersistentConversation: (
            characterId,
            conversationId,
            reason,
            replacement,
            options,
        ) =>
            coordinator.replacePersistentConversation(
                characterId,
                conversationId,
                reason,
                replacement,
                options,
            ),
        upsertPersistentCompleteCharacter: (
            characterId,
            reason,
            createOrMutate,
            options,
        ) =>
            withCompleteCharacter(characterId, () => coordinator.upsertPersistentCompleteCharacter(
                characterId,
                reason,
                createOrMutate,
                options,
            )),
        readPersistentCharacterDetail: (characterId, reason) =>
            coordinator.readPersistentCharacterDetail(characterId, reason),
        readPersistentCompleteCharacter: (characterId, reason) =>
            coordinator.readPersistentCompleteCharacter(characterId, reason),
        readPersistentConversation: (characterId, conversationId, reason) =>
            coordinator.readPersistentConversation(
                characterId,
                conversationId,
                reason,
            ),
        readPersistentConversationAt: (characterId, orderedPosition, reason) =>
            coordinator.readPersistentConversationAt(
                characterId,
                orderedPosition,
                reason,
            ),
        readPersistentSelectedConversation: (characterId, reason) =>
            coordinator.readPersistentSelectedConversation(characterId, reason),
        capturePersistentMutationToken: (reason, options) =>
            coordinator.capturePersistentMutationToken(reason, options),
        async acquireDestructiveReplacementFence(expected) {
            const owner = await coordinator.acquireDestructiveReplacementFence(expected)
            const heldRevision = coordinator.revision
            let released = false
            return {
                revision: heldRevision,
                refreshCommittedWorkingSet(revision, options) {
                    options = options && {
                        ...options,
                        changeSet: options.changeSet ? captureChanges(options.changeSet) : undefined,
                    }
                    if (released) {
                        return Promise.reject(
                            new Error(
                                'Destructive persistent replacement fence was released',
                            ),
                        )
                    }
                    return refreshCommittedWorkingSet(revision, owner, options)
                },
                release() {
                    if (released) return
                    coordinator.releaseDestructiveReplacementFence(owner)
                    released = true
                },
            }
        },
        acquireCommittedWorkingSetRefreshFence,
        materializePersistentDatabaseSnapshot: (reason) =>
            coordinator.materializePersistentDatabaseSnapshot(reason),
        materializePersistentDatabaseSnapshotWithRevision: (reason, options) =>
            coordinator.materializePersistentDatabaseSnapshotWithRevision(
                reason,
                options,
            ),
        async releaseInactiveWorkingSet(canRelease, isCurrent) {
            const token = await coordinator.capturePersistentMutationToken(
                'plugin-scalable-working-set',
            )
            const selectedCharacterId =
                dependencies.state.getSelectedCharacterId() ?? null
            const selectedConversationId =
                dependencies.state.getSelectedConversationId?.() ?? null
            const navigationGeneration = workingSet.navigationGenerationToken
            const database = await projectScalableWorkingSetAtRevision(
                dependencies.store,
                token.revision,
                {
                    selectedCharacterId,
                    selectedConversationId,
                    activeCharacterIds: workingSet.activeCharacterIds,
                },
            )
            const releaseAllowed = canRelease ? await canRelease() : true
            if (
                !releaseAllowed ||
                isCurrent?.() === false ||
                token.revision !== coordinator.revision ||
                token.mutationGeneration !== coordinator.mutationGeneration ||
                navigationGeneration !== workingSet.navigationGenerationToken ||
                selectedCharacterId !==
                    (dependencies.state.getSelectedCharacterId() ?? null) ||
                selectedConversationId !==
                    (dependencies.state.getSelectedConversationId?.() ?? null)
            )
                return false
            workingSet.invalidateNavigation()
            const activeCharacterIds = workingSet.reconcileActiveCharacterIds(
                database,
                selectedCharacterId,
            )
            dependencies.state.replaceDatabase(
                database,
                activeCharacterIds,
                true,
            )
            dependencies.state.restoreSelection?.(
                selectedCharacterId,
                selectedConversationId,
            )
            const resident = dependencies.state.captureSelectedCharacter()
            if (
                resident &&
                !coordinator.adoptHydratedCharacter(
                    token.revision,
                    token.mutationGeneration,
                    resident,
                )
            )
                return false
            return true
        },
        publishCurrentOfficialRevision: () =>
            coordinator.publishCurrentOfficialRevision(),
        hasPendingOfficialPublication: () =>
            coordinator.hasPendingOfficialPublication,
    }
    return runtime
}
