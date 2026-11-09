import { ConversationJournalConflictError } from './conversationJournalRebase'
import { advancePluginStorageRemoteChanges, fenceUnknownPluginStorageDrain } from '../plugins/pluginStorageProtection'
import type { PluginStorageWriteCheck } from '../plugins/pluginStorageProtection'
import { canonicalJson, canonicalClone } from './saveCoordinatorHelpers'
import {claimCommittedWorkingSetRecovery, continueCommittedWorkingSetRefresh, hasRetryableCommittedWorkingSetContinuation, rebaseCommittedWorkingSetContinuation, registerCommittedWorkingSetContinuation} from './committedWorkingSetContinuation'
import { diffRootMutations } from './rootMutation'
import { generatingConversations } from './generatingConversationRegistry'
import {
    NativeGenerationGuard,
    type NativeGenerationCloseResult,
    type NativeGenerationFenceReason,
    type NativeGenerationGuardEntry,
    type NativeGenerationGuardStatus,
    type NativeGenerationGuardTarget,
    type NativeGenerationGuardTicket,
    type NativeGenerationSettlement,
} from './nativeGenerationGuard'
import { NATIVE_GENERATION_STATUS_WAIT_MS, NativeGenerationUnknownTicketError, type NativeGenerationPort, type NativeGenerationProof, type NativeGenerationRouteStatus } from './nativeGenerationPort'
import { captureLwwWorkingSetBaseline, applyLwwWorkingSetUnits, readLwwReceiveRebaseKeys, prepareLwwReceiveDiscardBaseline } from './lwwWorkingSetApply'
import { translatePersistentRootUnitIntents } from './persistentIdentityHooks'
import type { LwwStageReceive, LwwStoredReceive, LwwApplyReceive, LwwApplyResult, PersistentUnitMutation, ConversationMutation, WholeMessageIntent, GeneratingConversation } from './persistentDataStore'
import { isTauri } from '../platform'
import type { PersistenceCanonicalCapture } from './reactivePersistenceCapture.svelte'
import type { Chat, Database, Message, botPreset, character } from './database.svelte'
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
    PersistentRevisionReader,
    PersistentRoot,
    PersistentConversationMetadata,
    PluginStorageMutation,
} from './persistentDataStore'
import { readCurrentRevision, RevisionConflictError } from './persistentDataStore'
import {
    SaveCoordinator,
    PersistentMutationFencedError,
    type CharacterAdditionRequest,
    type CapturedReceiveConversationEdits,
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
    type PreparedUnitIntent,
    type SaveCoordinatorClock,
    type SaveCoordinatorDependencies,
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
    createConversationSummaryFromMetadata,
    createConversationSummaryStub,
    createConversationSummaryStubFromChat,
    getConversationSummaryStub,
    isConversationSummaryStub,
} from './conversationResidency'
import { createMetadataOnlySelectedConversation, isMetadataOnlySelectedConversation } from './selectedConversationLifecycle'

type CompleteCharacter = character
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
    const index = database.characters.findIndex(
        (candidate) => candidate.chaId === state.characterId,
    )
    if (state.kind === 'delete') {

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
    /** Received root fields take the normalized value a load gives them. */
    fillRootFieldDefaults?(fields: readonly string[]): void
    /** Root fields whose working-set value changed when stored units were projected into it. */
    afterRemoteRootChange?(fields: ReadonlySet<string>): void
    /** A received plugin record changed which plugins run or how they are loaded. */
    afterRemotePluginChange?(): void
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
    onPluginDeviceStorageChanged?(owner?: string): void
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
    /** Gives a stored root read by a refresh the same preparation boot applies. */
    prepareRoot?(root: PersistentRoot): Promise<PersistentRoot>
    deletedCharacterAssets?: SaveCoordinatorDependencies['deletedCharacterAssets']
    /** The native generation commands, or null where the build has none. */
    getNativeGenerationPort?(): NativeGenerationPort | null
    /** Reads of native generation state before giving up, and the first backoff delay. */
    nativeGenerationRetry?: { readonly attempts: number; readonly delayMs: number }
}

export interface PersistentActivatedLibraryGuard {
    complete(): void
    abortUnchanged(): Promise<void>
}

export interface ActivatedLibraryRecoveryLifecycle {
    beforeRefresh(): Promise<number | void>
    afterRefresh(): Promise<void>
}

export interface PersistentUnitIntentProjectionReceipt {
    readonly revision: DataRevision
    readonly authorityEpoch: number
    readonly target: SelectedConversationTarget | null
    readonly session: ActiveConversationSession | null
    readonly sessionVersion: number | null
    readonly generationInvalidationVersion: number | null
}

export interface PersistentUnitIntentProjectionObserver {
    beforeProjection(): void
    afterProjection(receipt: PersistentUnitIntentProjectionReceipt): void
}

export interface PersistentReceiveDiscardAction {
    readonly characterId: string
    readonly conversationId: string
    discard(): Promise<CommittedApplyOutcome>
}

export interface PersistentDataRuntime {
    withPausedPersistentWrites<T>(reason: string, operation: (token: PersistentMutationToken) => Promise<T>): Promise<T>
    beginActivatedLibraryGuard(token: PersistentMutationToken): PersistentActivatedLibraryGuard
    setActivatedLibraryRecoveryLifecycle(token: PersistentMutationToken, lifecycle: ActivatedLibraryRecoveryLifecycle): void
    refreshActivatedLibraryUnderPause(token: PersistentMutationToken): Promise<CommittedApplyOutcome>
    applyLwwReceive(request: LwwStageReceive): Promise<LwwApplyResult>
    applyStoredLwwReceive(request: LwwStoredReceive, apply: (request: LwwApplyReceive) => Promise<LwwApplyResult>): Promise<LwwApplyResult>
    captureStoredReceiveDiscard(characterId: string, conversationId: string): PersistentReceiveDiscardAction | null
    drainLwwDeferred(expectedAuthorityEpoch?: number): Promise<void>
    commitPersistentUnitIntent(reason: string, mutations: readonly PersistentUnitMutation[], conversations?: readonly ConversationMutation[], wholeMessages?: readonly WholeMessageIntent[], projectionObserver?: PersistentUnitIntentProjectionObserver, check?: PluginStorageWriteCheck): Promise<void>
    /** Prepares at the revision the commit expects, through a reader pinned there; `null` commits nothing. */
    commitPreparedUnitIntent(reason: string, prepare: (reader: PersistentRevisionReader) => Promise<PreparedUnitIntent | null>): Promise<DataRevision | null>

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
    recordSelectedCharacterLastInteraction(authority: WindowedConversationPersistenceAuthority, before: number | undefined, after: number): boolean
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
        edit: (character: character) => string | null | false,
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
        check?: PluginStorageWriteCheck,
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
    deletePersistentCharacter(
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
    /** A revision committed while this fence is held is reported as a local revision on release. */
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

    /**
     * Guards a native generation's target from install until its committed state is
     * adopted. Synchronous, so it can run inside the paused-writes callback before execute.
     */
    assertNativeGenerationPreparationCurrent(token: PersistentMutationToken): void
    installNativeGenerationGuard(ticket: NativeGenerationGuardTicket, target: NativeGenerationGuardTarget, effectKeys?: readonly string[]): void
    setNativeGenerationEffectKeys(ticket: NativeGenerationGuardTicket, effectKeys: readonly string[]): boolean
    /** Records a status; a settled one releases the ticket, after adopting its committed output. Never call it inside the save queue. */
    settleNativeGeneration(status: NativeGenerationRouteStatus): Promise<NativeGenerationSettlement>
    /** Projects a proof's units into the working set in the save queue, without flushing first. */
    adoptNativeGenerationCommit(proof: NativeGenerationProof): Promise<void>
    /** The same adoption from inside the paused-writes callback that owns the token. */
    adoptNativeGenerationCommitInQueue(token: PersistentMutationToken, proof: NativeGenerationProof): Promise<void>
    /** Cancels every guarded ticket and waits up to the bound for settlement, then fences the rest. */
    closeNativeGenerations(boundMs: number): Promise<NativeGenerationCloseResult>
    getNativeGenerationGuards(): NativeGenerationGuardEntry[]
}

/** Why a native generation's committed state could not be adopted into the working set. */
export class NativeGenerationAdoptionError extends Error {
    constructor(readonly reason: 'unguarded' | 'proof-mismatch' | 'library-changed' | 'unplaceable-edit' | 'windowed-journal' | 'fenced', message = `Native generation adoption failed: ${reason}`) {
        super(message)
        this.name = 'NativeGenerationAdoptionError'
    }
}

export interface PersistentDestructiveReplacementFence {
    /** Revision the working set is pinned to while the fence is held. */
    readonly revision: DataRevision
    refreshCommittedWorkingSet(
        revision: DataRevision,
        options?: PersistentCommittedWorkingSetRefreshOptions,
    ): Promise<CommittedApplyOutcome>
    release(options?: { verifyStoreRevision: boolean }): void | Promise<void>
}

export interface PersistentCommittedWorkingSetRefreshOptions {
    forceScalableProjection?: boolean
    changeSet?: ReplacementChangeSet
}

function unitIntentAffectedKeys(mutations: readonly PersistentUnitMutation[], conversations: readonly ConversationMutation[]): string[] {
    const affectedKeys = mutations.map((value) => value.key)
    for (const value of conversations) {
        if (value.type === 'delete') affectedKeys.push(JSON.stringify(['exists', 'conversation', value.characterId, value.conversationId]))
        else if (value.type === 'reorder') affectedKeys.push(JSON.stringify(['order', 'conversations', value.characterId]))
        else affectedKeys.push(JSON.stringify(['messages', value.characterId, value.conversationId]))
    }
    return affectedKeys
}

const nativeTicketId = (ticket: NativeGenerationGuardTicket) => JSON.stringify([ticket.epoch, ticket.sequence])

/**
 * Replays edits held while native committed the target by `chatId`: rows changed or
 * removed since the persisted baseline are changed or removed in the committed history.
 * Throws when an edit has no `chatId` to place it by, such as a new or reordered row.
 */
function replayHeldMessageEdits(baseline: readonly Message[], live: readonly Message[], committed: readonly Message[]): Message[] {
    const idOf = (message: Message | undefined) => typeof message?.chatId === 'string' && message.chatId ? message.chatId : null
    const unique = (messages: readonly Message[]) => {
        const ids = messages.flatMap((message) => idOf(message) ?? [])
        return new Set(ids).size === ids.length
    }
    const unplaceable = () => new NativeGenerationAdoptionError('unplaceable-edit')
    if (!unique(baseline) || !unique(live) || !unique(committed)) throw unplaceable()
    const liveById = new Map(live.flatMap((message) => idOf(message) ? [[idOf(message)!, message] as const] : []))
    const edited = new Map<string, Message>()
    const removed = new Set<string>()
    for (const message of baseline) {
        const id = idOf(message)
        if (!id) continue
        const current = liveById.get(id)
        if (!current) removed.add(id)
        else if (canonicalJson(current) !== canonicalJson(message)) edited.set(id, current)
    }
    // The live rows must be exactly the baseline with those edits and removals.
    const explained = baseline.flatMap((message) => {
        const id = idOf(message)
        return id && removed.has(id) ? [] : [id ? edited.get(id) ?? message : message]
    })
    if (canonicalJson(explained) !== canonicalJson(live)) throw unplaceable()
    const committedIds = new Set(committed.flatMap((message) => idOf(message) ?? []))
    if ([...edited.keys(), ...removed].some((id) => !committedIds.has(id))) throw unplaceable()
    return committed.flatMap((message) => {
        const id = idOf(message)
        if (id && removed.has(id)) return []
        return [canonicalClone(id ? edited.get(id) ?? message : message)]
    })
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
    let pendingStoredReceiveRecovery: {
        retry(): Promise<CommittedApplyOutcome>
        captureDiscard(characterId: string, conversationId: string): PersistentReceiveDiscardAction
    } | null = null
    let lastStoredReceiveProjection: { bindingAuthority: string; requestId: string } | null = null
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
    // Native generation tickets this runtime guards: what their adoption rechecks, and the
    // windowed baseline a journal edit held on the target is rebased from.
    interface NativeTicketRecord {
        readonly target: NativeGenerationGuardTarget
        authorityEpoch: number
        readonly library: Promise<string | undefined>
        pin: Promise<PersistentRevisionLease | null> | null
        /** The windowed baseline revision `pin` leases, or null while the target is not the windowed selection. */
        pinRevision: DataRevision | null
    }
    const nativeTickets = new Map<string, NativeTicketRecord>()
    /**
     * Tickets a refresh retry adopts again: fenced ones, with the status, failure and reason of
     * their last fence, and held ones a write that lost the race to them twice is fenced behind.
     */
    const nativeRecoveries = new Map<string, {
        status: NativeGenerationRouteStatus | null
        error: unknown
        reason: NativeGenerationFenceReason | 'conflict'
    }>()
    /** The failure a fence raised outside an adoption carries instead of a generic one. */
    let nativeFenceCause: { error: unknown } | null = null
    /** Native's ticket list could not be read after a reload, with the last failure. */
    let nativeReinstallFailure: { error: unknown } | null = null
    const nativeRetryAttempts = Math.max(1, dependencies.nativeGenerationRetry?.attempts ?? 4)
    const nativeRetryDelayMs = Math.max(0, dependencies.nativeGenerationRetry?.delayMs ?? 500)
    const waitForNativeRetry = (attempt: number) => new Promise<void>((resolve) => {
        const delay = nativeRetryDelayMs * 2 ** attempt
        if (dependencies.clock) dependencies.clock.setTimeout(resolve, delay)
        else setTimeout(resolve, delay)
    })
    let nativeRecoveryOwner: symbol | null = null
    let adoptingNativeTarget: NativeGenerationGuardTarget | null = null
    const requireNativePort = (): NativeGenerationPort => {
        const port = dependencies.getNativeGenerationPort?.()
        if (!port) throw new Error('Native generation is unavailable')
        return port
    }
    const releaseLease = (lease: PersistentRevisionLease | null | undefined) => {
        if (lease) void releasePersistentRevisionLease(lease).catch((error) => dependencies.onBackgroundError?.(error))
    }
    const recordNativeTicket = (ticket: NativeGenerationGuardTicket, target: NativeGenerationGuardTarget) => {
        const id = nativeTicketId(ticket)
        if (nativeTickets.has(id)) return
        const library = Promise.resolve().then(() => dependencies.store.readLibraryIdentity?.())
        library.catch(() => undefined)
        const record: NativeTicketRecord = {
            target: { characterId: target.characterId, conversationId: target.conversationId },
            authorityEpoch: coordinator.storageAuthorityEpoch, library, pin: null, pinRevision: null,
        }
        nativeTickets.set(id, record)
        // A superseded baseline can no longer be leased, and native may commit before an edit is recorded.
        pinNativeBaseline(record)
    }
    /** Leases the target's current windowed baseline, which a held journal edit is rebased from. */
    const pinNativeBaseline = (record: NativeTicketRecord) => {
        const { characterId, conversationId } = record.target
        const revision = coordinator.windowedSelectedConversationBaselineRevision(characterId, conversationId)
        if (revision === record.pinRevision) return
        const previous = record.pin
        record.pinRevision = revision
        if (revision === null) {
            record.pin = null
            void previous?.then(releaseLease)
            return
        }
        const pin: Promise<PersistentRevisionLease | null> = (async () => {
            try {
                const lease = await dependencies.store.acquireRevision(revision)
                if (record.pin === pin && [...nativeTickets.values()].includes(record) &&
                    coordinator.windowedSelectedConversationBaselineRevision(characterId, conversationId) === revision) return lease
                releaseLease(lease)
            } catch {
                // Without the baseline, adopting over a held journal edit fences instead.
            }
            return null
        })()
        record.pin = pin
        void previous?.then(releaseLease)
    }
    /** The pin follows the windowed baseline while the store head still equals it. */
    const repinNativeBaselines = () => {
        if (adoptingNativeTarget) return
        for (const record of nativeTickets.values()) pinNativeBaseline(record)
    }
    const pruneNativeTickets = () => {
        const guarded = new Set(nativeGuard.snapshot().map((entry) => nativeTicketId(entry.ticket)))
        for (const [id, record] of nativeTickets) {
            if (guarded.has(id)) continue
            void record.pin?.then(releaseLease)
            record.pin = null
            record.pinRevision = null
            nativeTickets.delete(id)
            nativeRecoveries.delete(id)
        }
    }
    const trackNative = <T>(promise: Promise<T>): Promise<T> => {
        void promise.then(pruneNativeTickets, pruneNativeTickets)
        return promise
    }
    const nativeGuard: NativeGenerationGuard<NativeGenerationRouteStatus> = new NativeGenerationGuard<NativeGenerationRouteStatus>({
        cancel: (ticket, reason) => requireNativePort().cancel(ticket, reason),
        adopt: async (status) => {
            // Read before any await: a retry under the committed-refresh fence sets it for this call only.
            const owner = nativeRecoveryOwner
            const proof = await requireNativePort().proof(status.ticket)
            if (nativeTicketId(proof.ticket) !== nativeTicketId(status.ticket) || proof.at === 'issued') {
                throw new NativeGenerationAdoptionError('proof-mismatch')
            }
            if (owner) await adoptNativeCommit(proof, { owner })
            else await coordinator.enqueueWithoutFlush(() => adoptNativeCommit(proof, {}))
        },
        fence: (event) => {
            const error = event.error ?? nativeFenceCause?.error
            nativeRecoveries.set(nativeTicketId(event.ticket), { status: event.status, error, reason: event.reason })
            const committed = event.status?.committedRevision
            coordinator.markCommittedWorkingSetRefreshRequired(
                typeof committed === 'number' ? Math.max(coordinator.revision, committed) : coordinator.revision,
                error ?? new NativeGenerationAdoptionError('fenced', `Native generation was fenced: ${event.reason}`),
            )
        },
        onError: (error) => dependencies.onBackgroundError?.(error),
    })
    /** A journal edit of a guarded target cancels its ticket at once and pins the baseline it was made against. */
    const holdRecordedConversationMutation = (event: { characterId: string; conversationId: string }) => {
        const write = { conversations: [{ type: 'replace-range', characterId: event.characterId, conversationId: event.conversationId }] }
        if (!nativeGuard.holdWrite(write, 'edit')) return
        for (const record of nativeTickets.values()) {
            if (record.target.characterId === event.characterId && record.target.conversationId === event.conversationId) pinNativeBaseline(record)
        }
    }
    const acquireNativeAdoptionLease = async (minimum: DataRevision): Promise<PersistentRevisionLease> => {
        let revision = Math.max(minimum, await readCurrentRevision(dependencies.store))
        for (let attempt = 0; ; attempt++) {
            try {
                return await dependencies.store.acquireRevision(revision)
            } catch (error) {
                if (!(error instanceof RevisionConflictError) || error.actualRevision <= revision || attempt >= 7) throw error
                revision = error.actualRevision
            }
        }
    }
    /** Puts the target's held message edits onto the committed history in the projection's own turn. */
    const planHeldMessageReplay = async (
        database: Database,
        baseline: ReturnType<typeof captureLwwWorkingSetBaseline>,
        reader: PersistentRevisionReader,
        target: NativeGenerationGuardTarget,
    ): Promise<PersistentUnitIntentProjectionObserver | null> => {
        const owner = database.characters.find((value) => value.chaId === target.characterId)
        const live = owner && !isWorkingSetCharacterStub(owner)
            ? owner.chats.find((value) => value.id === target.conversationId) : undefined
        const before = baseline.characters.find((value) => value.chaId === target.characterId)
            ?.chats.find((value) => value.id === target.conversationId)
        if (!live || !before || !Object.hasOwn(before, 'message') || !Object.prototype.propertyIsEnumerable.call(live, 'message')) return null
        const planned = canonicalJson(live.message)
        if (planned === canonicalJson(before.message)) return null
        const committed = await reader.readConversation(target.characterId, target.conversationId)
        if (!committed) throw new NativeGenerationAdoptionError('unplaceable-edit')
        const messages = replayHeldMessageEdits(before.message, live.message, committed.value.message)
        return {
            beforeProjection() {
                if (canonicalJson(live.message) !== planned) throw new ConversationJournalConflictError('journal-changed')
                // The projection then sees a dirty row set and only moves the baseline to the committed rows.
                live.message = messages
            },
            afterProjection() {},
        }
    }
    /** A summary stub of the target keeps no rows to patch, so it takes the committed count instead. */
    const planTargetSummaryRefresh = async (database: Database, reader: PersistentRevisionReader, target: NativeGenerationGuardTarget) => {
        const findStub = () => {
            const owner = database.characters.find((value) => value.chaId === target.characterId)
            if (!owner || isWorkingSetCharacterStub(owner)) return null
            const index = owner.chats.findIndex((value) => value.id === target.conversationId)
            const summary = index < 0 ? null : getConversationSummaryStub(owner.chats[index])
            return summary ? { owner, index, summary } : null
        }
        if (!findStub()) return null
        const metadata = await reader.readConversationMetadata(target.characterId, target.conversationId)
        if (!metadata) return null
        return () => {
            const stub = findStub()
            if (!stub) return
            stub.owner.chats[stub.index] = createConversationSummaryStub(createConversationSummaryFromMetadata(target.characterId,
                metadata.value.conversation, stub.summary.configuredIndex, metadata.value.totalMessages, stub.summary.recentAt, stub.summary))
        }
    }
    const projectNativeCommit = async (proof: NativeGenerationProof, record: NativeTicketRecord, pin: PersistentRevisionLease | null, assertCurrent: () => void, owner?: symbol) => {
        const lease = await acquireNativeAdoptionLease(proof.revision)
        let leaseKept = false
        try {
            assertCurrent()
            const database = dependencies.state.captureWorkingSetDatabase?.()
            const baseline = database ? captureLwwWorkingSetBaseline(database, coordinator.capturePersistentBaselineRoot(), dependencies.state.capturePresets?.() ?? null, coordinator.captureMaterializedBaseline(), coordinator.capturePresetRecordBaseline()) : undefined
            const keys = new Set(proof.affectedUnitKeys)
            // Another writer committed after the proof: its units are merged as a receive would.
            if (database && baseline && lease.revision !== proof.revision && coordinator.revision < lease.revision) {
                for (const key of (await readLwwReceiveRebaseKeys(database, baseline, lease, coordinator.revision)).keys) keys.add(key)
            }
            const { characterId, conversationId } = record.target
            const messagesChanged = keys.has(JSON.stringify(['messages', characterId, conversationId]))
            const authority = workingSet.captureSelectedConversationAuthority()
            const windowed = authority?.characterId === characterId && authority.conversationId === conversationId ? authority : null
            const baselineLease = windowed && pin && pin.revision === coordinator.windowedSelectedConversationBaselineRevision(characterId, conversationId) ? pin : undefined
            // A held journal edit is rebased only from the baseline it was recorded against.
            if (windowed && messagesChanged && windowed.sessionVersion !== windowed.persistedSessionVersion && !baselineLease) {
                throw new NativeGenerationAdoptionError('windowed-journal')
            }
            const replay = database && baseline && messagesChanged ? await planHeldMessageReplay(database, baseline, lease, record.target) : null
            const refreshSummary = database && messagesChanged ? await planTargetSummaryRefresh(database, lease, record.target) : null
            const observer: PersistentUnitIntentProjectionObserver | undefined = replay || refreshSummary ? {
                beforeProjection: () => replay?.beforeProjection(),
                afterProjection: (receipt) => {
                    replay?.afterProjection(receipt)
                    refreshSummary?.()
                },
            } : undefined
            assertCurrent()
            adoptingNativeTarget = record.target
            try {
                await projectAppliedUnits({ revision: lease.revision, affectedKeys: [...keys], heldKeys: [], deferredKeys: [] }, baseline, false, true,
                    observer, { owner, lease, baselineLease, allowLocalFields: true })
            } finally {
                adoptingNativeTarget = null
            }
            // A ticket that keeps running is rebased from this revision next.
            if (proof.at === 'issued' && coordinator.windowedSelectedConversationBaselineRevision(characterId, conversationId) === lease.revision) {
                const previous = record.pin
                record.pin = Promise.resolve(lease)
                record.pinRevision = lease.revision
                leaseKept = true
                void previous?.then(releaseLease)
            }
        } finally {
            if (!leaseKept) await releasePersistentRevisionLease(lease)
        }
    }
    const adoptNativeCommit = async (proof: NativeGenerationProof, mode: { owner?: symbol; token?: PersistentMutationToken }): Promise<void> => {
        const record = nativeTickets.get(nativeTicketId(proof.ticket))
        if (!record) throw new NativeGenerationAdoptionError('unguarded')
        const assertCurrent = () => {
            if (mode.owner) return coordinator.assertDestructiveReplacementFence(mode.owner)
            coordinator.assertPersistentMutationAllowed(record.authorityEpoch)
            if (mode.token) coordinator.assertPausedPersistentWrites(mode.token)
        }
        assertCurrent()
        const library = await record.library
        if (library !== undefined && await dependencies.store.readLibraryIdentity?.() !== library) {
            throw new NativeGenerationAdoptionError('library-changed')
        }
        for (let attempt = 0; ; attempt++) {
            assertCurrent()
            const pin = await record.pin
            try {
                await projectNativeCommit(proof, record, pin ?? null, assertCurrent, mode.owner)
                break
            } catch (error) {
                if (!(error instanceof ConversationJournalConflictError) || error.reason !== 'journal-changed' || attempt >= 7) throw error
            }
        }
        if (proof.at === 'issued') {
            // Pre-dispatch effects no longer race; only the target conversation stays guarded.
            nativeGuard.setEffectKeys(proof.ticket, [])
            // Repinning paused while the projection moved the baseline.
            pinNativeBaseline(record)
        }
    }
    const settledNativeStatus = (status: NativeGenerationGuardStatus) =>
        status.phase !== 'reserved' && status.phase !== 'running' && !status.pendingCommit
    /** The settled status that stops guarding a ticket whose output native cannot report. */
    const unavailableNativeStatus = (
        guarded: Pick<NativeGenerationRouteStatus, 'ticket' | 'characterId' | 'conversationId'>,
        last: NativeGenerationRouteStatus | null,
    ): NativeGenerationRouteStatus => ({
        ...last, ticket: guarded.ticket, characterId: guarded.characterId, conversationId: guarded.conversationId,
        phase: 'interrupted', pendingCommit: false, output: 'unavailable',
    })
    /** Runs a guard call whose adoption, if it starts one, projects under the refresh fence `owner` holds. */
    const adoptUnderRefreshFence = (owner: symbol, run: () => Promise<NativeGenerationSettlement>) => {
        nativeRecoveryOwner = owner
        try { return run() }
        finally { nativeRecoveryOwner = null }
    }
    /**
     * One recovery attempt: adopted, still running, failed for now (kept fenced with the held
     * edits), or beyond adoption.
     */
    const attemptNativeRecovery = async (port: NativeGenerationPort, id: string, entry: NativeGenerationGuardEntry, owner: symbol): Promise<'adopted' | 'pending' | 'transient' | 'abandon' | 'gone'> => {
        let status: NativeGenerationRouteStatus
        try {
            status = await port.status(entry.ticket)
        } catch (error) {
            if (error instanceof NativeGenerationUnknownTicketError) return 'abandon'
            const recovery = nativeRecoveries.get(id)
            if (recovery) recovery.error = error
            return 'transient'
        }
        // A held ticket adopts as it settles; a fenced one records native's current status first.
        let settlement = await adoptUnderRefreshFence(owner, () => nativeGuard.settle(status))
        if (settlement === 'pending') return 'pending'
        if (entry.state === 'fenced' && settlement === 'fenced') {
            if (!settledNativeStatus(status)) return 'pending'
            if (status.output === 'unavailable') return 'abandon'
            settlement = await adoptUnderRefreshFence(owner, () => nativeGuard.retryAdoption(entry.ticket))
        } else if (settlement === 'fenced' && status.output === 'unavailable') {
            return 'abandon'
        }
        if (settlement === 'released') return 'adopted'
        if (settlement !== 'fenced') return 'gone'
        // Held edits that cannot be placed, a lost pin or another library stay so on every retry.
        const error = nativeRecoveries.get(id)?.error
        return error instanceof NativeGenerationAdoptionError && error.reason !== 'fenced' ? 'abandon' : 'transient'
    }
    /** Stops guarding a fenced ticket whose committed state cannot be adopted; a refresh reloads its target. */
    const abandonNativeRecovery = async (port: NativeGenerationPort, id: string, unsettled: boolean) => {
        const last = nativeRecoveries.get(id)?.status ?? null
        nativeRecoveries.delete(id)
        const entry = nativeGuard.snapshot().find((value) => nativeTicketId(value.ticket) === id)
        if (!entry) return
        // A ticket native may still run is asked to stop before its target reloads.
        if (unsettled) void port.cancel(entry.ticket, 'authority').catch((error) => dependencies.onBackgroundError?.(error))
        await nativeGuard.settle(unavailableNativeStatus(entry, last))
        nativeGuard.release(entry.ticket)
    }
    /**
     * Adopts again, under the committed-refresh fence, every ticket a fence waits on. A ticket
     * whose committed state cannot be adopted, or one native never settled after the close bound,
     * stops guarding its target, and `reload` asks the committed refresh to load that target from
     * the store. A failure that can pass keeps the fence and the held edits for the next retry.
     */
    const retryNativeAdoptions = async (): Promise<CommittedApplyOutcome | 'reload'> => {
        const owner = await coordinator.acquireCommittedWorkingSetRefreshFence()
        let reload = false
        try {
            const port = requireNativePort()
            for (const id of [...nativeRecoveries.keys()]) {
                let result: Awaited<ReturnType<typeof attemptNativeRecovery>> = 'transient'
                for (let attempt = 0; (result === 'transient' || result === 'pending') && attempt < nativeRetryAttempts; attempt++) {
                    if (attempt > 0) await waitForNativeRetry(attempt - 1)
                    const entry = nativeGuard.snapshot().find((value) => nativeTicketId(value.ticket) === id)
                    result = entry ? await attemptNativeRecovery(port, id, entry, owner) : 'gone'
                }
                if (result === 'adopted') {
                    nativeRecoveries.delete(id)
                    // Each adoption moves the baselines the next one is checked against.
                    coordinator.completeCommittedUnitProjection(owner)
                } else if (result === 'gone') {
                    nativeRecoveries.delete(id)
                } else if (result === 'abandon' || (result === 'pending' && nativeRecoveries.get(id)?.reason === 'close-timeout')) {
                    await abandonNativeRecovery(port, id, result === 'pending')
                    reload = true
                }
            }
            pruneNativeTickets()
            const remaining = [...nativeRecoveries.values()].pop()
            if (remaining) return requireCommittedRefresh(coordinator.revision, remaining.error ?? new NativeGenerationAdoptionError('fenced'))
            if (reload || coordinator.pendingWorkingSetRefreshRevision !== null) return 'reload'
            return { kind: 'committed', revision: coordinator.revision, projection: 'applied' }
        } catch (error) {
            return requireCommittedRefresh(coordinator.revision, error)
        } finally {
            coordinator.releaseDestructiveReplacementFence(owner)
        }
    }
    /**
     * Fenced tickets whose target a full committed refresh reloaded from the store stop guarding
     * it, and a held ticket no longer has a fenced write behind it.
     */
    const releaseRefreshedNativeGenerations = () => {
        for (const entry of nativeGuard.snapshot()) {
            if (entry.state !== 'fenced' || nativeGuard.release(entry.ticket)) nativeRecoveries.delete(nativeTicketId(entry.ticket))
        }
        pruneNativeTickets()
    }
    /** Drives a ticket native still lists after a reload until it settles. */
    const resumeNativeGeneration = (initial: NativeGenerationRouteStatus) => {
        const id = nativeTicketId(initial.ticket)
        void (async () => {
            let status = initial
            let failures = 0
            while (nativeTickets.has(id)) {
                if (await trackNative(nativeGuard.settle(status)) !== 'pending') return
                try {
                    status = await requireNativePort().status(status.ticket, NATIVE_GENERATION_STATUS_WAIT_MS)
                    failures = 0
                } catch (error) {
                    if (!(error instanceof NativeGenerationUnknownTicketError) && ++failures < nativeRetryAttempts) {
                        await waitForNativeRetry(failures - 1)
                        continue
                    }
                    // The hold becomes the committed-refresh fence, which a refresh retry resolves;
                    // the fence reports why native's status could not be read.
                    nativeFenceCause = { error }
                    let settling: Promise<NativeGenerationSettlement>
                    try { settling = trackNative(nativeGuard.settle(unavailableNativeStatus(status, status))) }
                    finally { nativeFenceCause = null }
                    await settling
                    return
                }
            }
        })().catch((error) => dependencies.onBackgroundError?.(error))
    }
    /** Guards again the tickets native still lists; false when the list could not be read. */
    const reinstallNativeGenerations = async (): Promise<boolean> => {
        const port = dependencies.getNativeGenerationPort?.()
        if (!port) return true
        let active: readonly NativeGenerationRouteStatus[] | null = null
        for (let attempt = 0; !active && attempt < nativeRetryAttempts; attempt++) {
            if (attempt > 0) await waitForNativeRetry(attempt - 1)
            try {
                active = await port.active()
            } catch (error) {
                nativeReinstallFailure = { error }
            }
        }
        if (!active) return false
        nativeReinstallFailure = null
        nativeGuard.reinstall(active.map((status) => ({ status, characterId: status.characterId, conversationId: status.conversationId })))
        for (const status of active) recordNativeTicket(status.ticket, status)
        for (const status of active) resumeNativeGeneration(status)
        return true
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
                characterIds: [result.characterId],
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
        onWindowedSelectedConversationRevision: (revision, totalMessages, preserveRows) => {
            workingSet.advanceStoreRevision(revision, totalMessages, preserveRows)
            repinNativeBaselines()
        },
        onConversationMutationRecorded: holdRecordedConversationMutation,
        nativeGenerationGuard: nativeGuard,
        onNativeGenerationConflictFenced: (tickets, error) => {
            for (const ticket of tickets) {
                const id = nativeTicketId(ticket)
                if (!nativeRecoveries.has(id)) nativeRecoveries.set(id, { status: null, error, reason: 'conflict' })
            }
        },
        completeWindowedSelectedConversation: (authority, messages) =>
            workingSet.completeWindowedConversationForSave(authority, messages),
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
        deletedCharacterAssets: dependencies.deletedCharacterAssets,
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
        publishConversation: dependencies.state.publishConversation,
        captureActivationRollback: dependencies.state.captureActivationRollback,
        canActivateWorkingSet: () =>
            coordinator.pendingWorkingSetRefreshRevision === null &&
            dependencies.state.canActivateWorkingSet?.() !== false,
        canDeactivateWorkingSet: () =>
            coordinator.pendingWorkingSetRefreshRevision === null &&
            dependencies.state.canDeactivateWorkingSet?.() !== false,
        canDeactivateCharacter: dependencies.state.canDeactivateCharacter,
        releaseInactiveCharacter: dependencies.state.releaseInactiveCharacter
            ? (id) => {
                dependencies.state.releaseInactiveCharacter!(id)
                coordinator.forgetMaterializedCharacter(id)
            }
            : undefined,
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
        const onRetryExhausted = options?.onRetryExhausted
        if (!prepare) {
            return workingSet.activateCharacter(
                id,
                normalize || onRetryExhausted ? { normalize, onRetryExhausted } : undefined,
            )
        }
        return workingSet.activateCharacter(id, {
            normalize,
            onRetryExhausted,
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
            // the next window an idempotent replay, so the user has nothing to act on.
            console.warn('The content change cursor was not committed', error)
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
                                prepareRoot: dependencies.prepareRoot,
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
                database: await projectPinnedScalableWorkingSet(lease, {
                    ...pinned,
                    keepConversation,
                    prepareRoot: dependencies.prepareRoot,
                }),
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
    const applyInstalledRootChanges = (before: RootDatabase): void => {
        const changedFields = new Set(diffRootMutations(before, dependencies.state.captureRoot()).map((mutation) => mutation.key))
        if (changedFields.size === 0) return
        try { dependencies.state.afterRemoteRootChange?.(changedFields) }
        catch (error) { dependencies.onBackgroundError?.(error) }
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
            const previousAuthority = coordinator.storageAuthorityEpoch
            const nativeRefreshTickets = activatedLibraryGuard ? [] : nativeGuard.snapshot()
                .filter(entry => entry.state === 'holding')
                .flatMap(entry => {
                    const id = nativeTicketId(entry.ticket)
                    const record = nativeTickets.get(id)
                    return record?.authorityEpoch === previousAuthority ? [{ id, record, pin: record.pin }] : []
                })
            const refreshLibrary = nativeRefreshTickets.length ? await dependencies.store.readLibraryIdentity?.() : undefined
            const nativeRefreshCandidates = (await Promise.all(nativeRefreshTickets.map(async value => ({
                ...value, library: await value.record.library,
            })))).filter(value => value.library !== undefined && value.library === refreshLibrary)
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
            const rootBefore = canonicalClone(dependencies.state.captureRoot())
            workingSet.invalidateNavigation()
            dependencies.state.replaceDatabase(
                projected.database,
                activeCharacterIds,
                options?.forceScalableProjection ?? true,
            )
            let installed = projected.database
            if (activatedLibraryGuard) {
                // The activated library has no resident character to keep selected,
                // and its derived identity mirrors belong to the installed baseline.
                dependencies.state.restoreSelection?.(null, null)
                dependencies.state.afterRemoteApply?.()
            }
            installed = dependencies.state.captureWorkingSetDatabase?.() ?? installed
            const holdingBeforeInstall = new Set(nativeGuard.snapshot().filter(entry => entry.state === 'holding').map(entry => nativeTicketId(entry.ticket)))
            const sameNativeTickets = nativeRefreshCandidates.filter(({ id, record, pin }) =>
                holdingBeforeInstall.has(id) && nativeTickets.get(id) === record && record.pin === pin &&
                record.authorityEpoch === previousAuthority && coordinator.storageAuthorityEpoch === previousAuthority)
            workingSet.installCommittedWorkingSet(installed, revision, projected.windowedMetadata)
            const installedAuthority = coordinator.storageAuthorityEpoch
            const refreshedNativeTickets = sameNativeTickets.map(value => ({ ...value, pin: value.record.pin }))
            if (!projected.deferred && refreshedNativeTickets.length) coordinator.completeCommittedUnitProjection(fenceOwner)
            applyInstalledRootChanges(rootBefore)
            deferredContentPending = projected.deferred
            if (!projected.deferred) releaseRefreshedNativeGenerations()
            if (!projected.deferred) await commitContentCursor(revision, activatedLibraryGuard !== null)
            if (activatedLibraryGuard) {
                await activatedLibraryGuard.lifecycle?.afterRefresh()
                coordinator.finishActivatedLibraryGuard(activatedLibraryGuard.owner)
                activatedLibraryGuard = null
            }
            if (!projected.deferred && refreshedNativeTickets.length) {
                const library = await dependencies.store.readLibraryIdentity?.()
                coordinator.assertDestructiveReplacementFence(fenceOwner)
                if (coordinator.storageAuthorityEpoch !== installedAuthority || library !== refreshLibrary) {
                    throw new NativeGenerationAdoptionError('library-changed')
                }
                const holding = new Set(nativeGuard.snapshot().filter(entry => entry.state === 'holding').map(entry => nativeTicketId(entry.ticket)))
                for (const { id, record, pin } of refreshedNativeTickets) {
                    if (!holding.has(id) || nativeTickets.get(id) !== record || record.pin !== pin || record.authorityEpoch !== previousAuthority) continue
                    // A successful refresh changes the working-set epoch, not this ticket's library.
                    record.authorityEpoch = installedAuthority
                    pinNativeBaseline(record)
                }
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
                        const latestRevision = await readCurrentRevision(dependencies.store)
                        coordinator.assertDestructiveReplacementFence(owner)
                        const minimum = Math.max(
                            minimumRevision,
                            coordinator.pendingWorkingSetRefreshRevision ?? minimumRevision,
                            typeof reconciledRevision === 'number' ? reconciledRevision : minimumRevision,
                        )
                        if (latestRevision < minimum) {
                            throw new RevisionConflictError(minimum, latestRevision)
                        }
                        return refreshCommittedWorkingSet(latestRevision, owner, {
                            ...options,
                            changeSet: latestRevision === minimumRevision ? options?.changeSet : undefined,
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
    // A native target being adopted is generating only toward the state adoption installs.
    const generating = () => [...(dependencies.state.getGeneratingConversations?.() ?? generatingConversations.snapshot())]
        .filter((owner) => owner.characterId !== adoptingNativeTarget?.characterId || owner.conversationId !== adoptingNativeTarget.conversationId)
    /// Keeps the selection on the same character and conversation after applied
    /// units moved or removed them. A removed character clears the selection and a
    /// removed conversation falls back to the first remaining one, as local removal
    /// does. Returns the conversation that still has to be opened.
    const reconcileAppliedSelection = (
        database: Database,
        pinned: { characterId: string | null; conversationId: string | null },
    ): string | null => {
        if (!pinned.characterId) return null
        const character = database.characters.find((value) => value.chaId === pinned.characterId)
        if (!character) {
            dependencies.state.restoreSelection?.(null, null)
            workingSet.invalidateNavigation()
            workingSet.reconcileActiveCharacterIds(database, null)
            coordinator.releaseRemovedSelectedConversation(true)
            return null
        }
        dependencies.state.restoreSelection?.(pinned.characterId, pinned.conversationId)
        if (!pinned.conversationId || character.chats.some((value) => value.id === pinned.conversationId)) return null
        character.chatPage = 0
        workingSet.invalidateNavigation()
        coordinator.releaseRemovedSelectedConversation(false)
        return character.chats[0]?.id ?? null
    }
    const projectAppliedUnits = async (result: LwwApplyResult, baseline?: ReturnType<typeof captureLwwWorkingSetBaseline>, localIntent = false, preserveWindowMetadata = false, projectionObserver?: PersistentUnitIntentProjectionObserver, recovery?: { owner?: symbol; lease: PersistentRevisionLease; baselineLease?: PersistentRevisionLease; allowLocalFields: boolean; journalPublished?(): void; coordinatorPublished?(): void; discard?: {
        captured: CapturedReceiveConversationEdits; publication: ReturnType<CapturedReceiveConversationEdits['prepareDiscard']>
    } }): Promise<void> => {
        if (recovery?.owner) coordinator.assertDestructiveReplacementFence(recovery.owner)
        if (!localIntent && result.affectedKeys.length === 0 && !recovery) {
            // Nothing reached the library, so the working set and its mirrors stay untouched.
            coordinator.adoptAppliedUnitState(result.revision, null, null, [], [], undefined, true)
            await commitContentCursor(result.revision)
            return
        }
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
        const refreshSelectedSession = () => {
            if (selectedFields && [...selectedFields].some(([field, value]) => (selectedChat as unknown as Record<string, unknown>)[field] !== value) &&
                selectedSession!.matchesConversation(selectedTarget!.characterId, selectedChat!) &&
                !selectedSession!.adoptPersistedMetadata(selectedChat!, result.revision)) {
                const index = selectedOwner!.chats.indexOf(selectedChat!)
                if (index >= 0) selectedOwner!.chats[index] = { ...selectedChat! }
            }
            if (selectedTarget && selectedSession) workingSet.refreshSelectedConversationAfterReplacement(selectedTarget, selectedSession)
        }
        const lease = recovery?.lease ?? await dependencies.store.acquireRevision(result.revision)
        const projectionBaseline = baseline
        const journalAuthority = workingSet.captureSelectedConversationAuthority()
        const assertJournalPublicationCurrent = recovery?.baselineLease ? coordinator.captureWindowedJournalPublicationGuard() : () => {}
        const journalPlan = recovery?.baselineLease && !recovery.discard
            ? await coordinator.prepareWindowedJournalRebase(recovery.baselineLease, lease) : null
        let journalAdoption: ReturnType<ActiveWorkingSet['prepareWindowedJournalAdoption']> | null = null
        let discardSelection: Awaited<ReturnType<ActiveWorkingSet['prepareReceiveConversationDiscard']>> | null = null
        const discardedOwners = new Set<string>()
        const discardedConversations: Array<{ characterId: string; conversationId: string }> = []
        const rootFields = [...new Set(result.affectedKeys.flatMap((key) => {
            const [kind, field] = JSON.parse(key) as string[]
            return kind === 'root' ? [field] : []
        }))]
        const rootValue = (field: string) => canonicalJson({ value: (database as unknown as Record<string, unknown>)[field] })
        let rootBefore: string[] = []
        const receivedPlugins = localIntent ? [] : [...new Set(result.affectedKeys.flatMap((key) => {
            const [kind, collection, name] = JSON.parse(key) as string[]
            return kind === 'record' && collection === 'plugins' ? [name] : []
        }))]
        const pluginLoadInputs = () => canonicalJson(receivedPlugins.map((name) => {
            const plugin = database.plugins?.find((value) => value.name === name)
            return plugin ? [plugin.enabled, plugin.script, plugin.version, plugin.arguments, plugin.realArg] : null
        }))
        let pluginsBefore = ''
        let pluginsChanged = false
        // Membership and order units can move the selected entries away from their indexes.
        const reselect = result.affectedKeys.some((key) => {
            const [kind, id] = JSON.parse(key) as string[]
            return (recovery !== undefined && kind === 'archive') || (kind === 'exists' ? id === 'character' || id === 'conversation' : kind === 'order' && id === 'conversations')
        })
        let pinnedSelection: { characterId: string | null; conversationId: string | null } | null = null
        let reopenConversation: string | null = null
        let applied: ReturnType<typeof captureLwwWorkingSetBaseline>
        let adoptWindowedMetadata = () => {}
        let windowedConversation: { characterId: string; conversationId: string; totalMessages: number } | undefined
        try {
            journalAdoption = journalPlan ? workingSet.prepareWindowedJournalAdoption(journalPlan) : null
            if (recovery) {
                for (const key of result.affectedKeys) {
                    const [kind, scope, characterId, conversationId] = JSON.parse(key) as string[]
                    if (kind === 'archive' && (await lease.readCharacterSummary(scope))?.archived) discardedOwners.add(scope)
                    if (kind === 'exists' && scope === 'character' && !await lease.readCharacterSummary(characterId)) discardedOwners.add(characterId)
                    if (kind === 'exists' && scope === 'conversation' && !await lease.readConversationMetadata(characterId, conversationId)) discardedConversations.push({ characterId, conversationId })
                }
            }
            discardSelection = recovery?.discard ? await workingSet.prepareReceiveConversationDiscard(lease,
                recovery.discard.captured.characterId, recovery.discard.captured.conversationId,
                result.affectedKeys.includes(JSON.stringify(['messages', recovery.discard.captured.characterId, recovery.discard.captured.conversationId]))) : null
            if (!recovery?.discard && preserveWindowMetadata && selectedTarget && (!recovery || !result.affectedKeys.includes(JSON.stringify(['archive', selectedTarget.characterId]))) && result.affectedKeys.some((key) => {
                const [kind, characterId, conversationId] = JSON.parse(key) as string[]
                return kind === 'messages' && characterId === selectedTarget.characterId && conversationId === selectedTarget.conversationId
            })) adoptWindowedMetadata = await workingSet.prepareWindowedMetadataAdoption(lease)
            applied = await applyLwwWorkingSetUnits(database, projectionBaseline, lease, result.affectedKeys, dependencies.state.captureCharacterIndex?.(), localIntent || recovery?.allowLocalFields === true, () => {
                if (recovery?.owner) coordinator.assertDestructiveReplacementFence(recovery.owner)
                else coordinator.assertPersistentMutationAllowed(authorityEpoch)
                if (dependencies.state.captureWorkingSetDatabase?.() !== database) throw new Error('Persistent working set changed during unit projection')
                dependencies.state.beforeCapture?.()
                assertJournalPublicationCurrent()
                if (recovery?.discard) {
                    recovery.discard.publication.assertRemovalCovered(discardedOwners, discardedConversations)
                    discardSelection!.assertCurrent()
                    if (generating().some(owner => owner.characterId === recovery.discard!.captured.characterId)) {
                        throw new ConversationJournalConflictError('active-generation')
                    }
                } else if (recovery) coordinator.assertReceiveRemovalHasNoPendingEdits(discardedOwners, discardedConversations)
                if (journalPlan) {
                    journalPlan.assertCurrent()
                    if (!journalPlan.historyUnchanged && generating().some(owner => owner.characterId === journalPlan.authority.characterId && owner.conversationId === journalPlan.authority.conversationId)) {
                        throw new ConversationJournalConflictError('active-generation')
                    }
                } else if (recovery && journalAuthority && workingSet.captureSelectedConversationAuthority()?.sessionVersion !== journalAuthority.sessionVersion) {
                    throw new ConversationJournalConflictError('journal-changed')
                }
                if (recovery && result.affectedKeys.some(key => {
                    const [kind, id] = JSON.parse(key) as string[]
                    return kind === 'archive' && generating().some(owner => owner.characterId === id)
                })) throw new Error('Received archive conflicts with active generation')
                if (recovery && result.affectedKeys.some(key => {
                    const [kind, scope, characterId, conversationId] = JSON.parse(key) as string[]
                    return kind === 'exists' && generating().some(owner => owner.characterId === characterId &&
                        (scope === 'character' || scope === 'conversation' && owner.conversationId === conversationId))
                })) throw new ConversationJournalConflictError('active-generation')
                rootBefore = rootFields.map(rootValue)
                if (receivedPlugins.length) pluginsBefore = pluginLoadInputs()
                if (reselect) pinnedSelection = {
                    characterId: dependencies.state.getSelectedCharacterId() ?? null,
                    conversationId: dependencies.state.getSelectedConversationId?.() ?? null,
                }
                projectionObserver?.beforeProjection()
            }, () => {
                if (journalPlan && journalAdoption) {
                    const persisted = projectionBaseline.characters.find(character => character.chaId === journalPlan.authority.characterId)
                    if (!persisted) throw new ConversationJournalConflictError('missing-history')
                    journalPlan.adopt(persisted)
                    journalAdoption.publish()
                }
                if (recovery?.baselineLease && !recovery.discard) recovery.journalPublished?.()
                // Corrected in the same turn as the projection, before any view reads the indexes.
                if (pinnedSelection && !recovery?.discard) reopenConversation = reconcileAppliedSelection(database, pinnedSelection)
                if (discardSelection && recovery?.discard) {
                    discardSelection.publish()
                    recovery.discard.publication.publish(projectionBaseline.characters.find(character => character.chaId === recovery.discard!.captured.characterId) ?? null, discardSelection.authority)
                    const authority = discardSelection.authority
                    coordinator.adoptAppliedUnitState(result.revision, projectionBaseline.root, projectionBaseline.presets,
                        projectionBaseline.characters, projectionBaseline.presetRecords, authority ? {
                            characterId: authority.characterId, conversationId: authority.conversationId, totalMessages: authority.totalMessages,
                        } : undefined)
                    recovery.journalPublished?.()
                    recovery.coordinatorPublished?.()
                    if (pinnedSelection) reopenConversation = reconcileAppliedSelection(database, pinnedSelection)
                    discardSelection.notify()
                }
                pluginsChanged = receivedPlugins.length > 0 && pluginLoadInputs() !== pluginsBefore
                const canonicalCapture = dependencies.state.canonicalCapture
                const beforeDerive = canonicalCapture?.root() ?? canonicalJson(dependencies.state.captureRoot())
                if (rootFields.length > 0) dependencies.state.fillRootFieldDefaults?.(rootFields)
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
                if (recovery?.discard) {
                    const authority = discardSelection!.authority
                    coordinator.adoptAppliedUnitState(result.revision, projectionBaseline.root, projectionBaseline.presets,
                        projectionBaseline.characters, projectionBaseline.presetRecords, authority ? {
                            characterId: authority.characterId, conversationId: authority.conversationId, totalMessages: authority.totalMessages,
                        } : undefined)
                }
                if (projectionObserver) {
                    // Capture ownership in the publication turn, before lease release or cursor I/O can yield.
                    refreshSelectedSession()
                    const session = workingSet.activeConversationSession
                    projectionObserver.afterProjection({
                        revision: result.revision,
                        authorityEpoch,
                        target: workingSet.captureSelectedConversationTarget(),
                        session,
                        sessionVersion: session?.version ?? null,
                        generationInvalidationVersion: session?.generationInvalidationVersion ?? null,
                    })
                }
            }, recovery !== undefined)
            // A windowed shell holds no messages to patch; its row source needs the received count instead.
            if (!recovery?.discard && selectedTarget && workingSet.selectedConversationMode === 'windowed' && (!recovery || !result.affectedKeys.includes(JSON.stringify(['archive', selectedTarget.characterId]))) && result.affectedKeys.some((key) => {
                const [kind, characterId, conversationId] = JSON.parse(key) as string[]
                return kind === 'messages' && characterId === selectedTarget.characterId && conversationId === selectedTarget.conversationId
            })) {
                const stored = await lease.readConversationMetadata(selectedTarget.characterId, selectedTarget.conversationId)
                if (stored) windowedConversation = { characterId: selectedTarget.characterId, conversationId: selectedTarget.conversationId, totalMessages: stored.value.totalMessages }
            }
        } finally {
            journalAdoption?.dispose()
            discardSelection?.dispose()
            if (!recovery) await releasePersistentRevisionLease(lease)
        }
        const preserveSelectedRows = Boolean(selectedTarget) && result.affectedKeys.every((key) => {
            const [kind, owner, id] = JSON.parse(key) as string[]
            if (kind === 'root' || kind === 'preset' || kind === 'preset-protected' || kind === 'record' || kind === 'toggle' || kind === 'variable') return false
            if (kind === 'character' || kind === 'conversation' || kind === 'messages') return owner !== selectedTarget!.characterId
            if (kind === 'order') return owner === 'conversations' && id !== selectedTarget!.characterId
            return kind === 'exists' && owner === 'character' && id !== selectedTarget!.characterId
        })
        if (!recovery?.discard) {
            coordinator.adoptAppliedUnitState(result.revision, applied.root, applied.presets, applied.characters, applied.presetRecords, windowedConversation, preserveSelectedRows)
            recovery?.coordinatorPublished?.()
        }
        for (const key of result.affectedKeys) {
            const [kind, id] = JSON.parse(key) as string[]
            if (recovery && kind === 'archive') coordinator.forgetMaterializedCharacter(id)
        }
        adoptWindowedMetadata()
        const changedRootFields = new Set(rootFields.filter((field, index) => rootValue(field) !== rootBefore[index]))
        if (changedRootFields.size > 0) {
            try { dependencies.state.afterRemoteRootChange?.(changedRootFields) }
            catch (error) { dependencies.onBackgroundError?.(error) }
        }
        if (pluginsChanged) {
            try { dependencies.state.afterRemotePluginChange?.() }
            catch (error) { dependencies.onBackgroundError?.(error) }
        }
        if (!projectionObserver && !recovery?.discard) refreshSelectedSession()
        for (const key of result.affectedKeys) {
            const [kind, owner, name] = JSON.parse(key)
            if (kind === 'plugin') dependencies.state.onPluginStorageChanged?.(owner, name)
            else if (kind === 'plugin-local') dependencies.state.onPluginDeviceStorageChanged?.(owner)
            else if (kind === 'order' && owner === 'plugin-storage') {
                dependencies.state.onPluginStorageChanged?.(name, '')
            }
        }
        await commitContentCursor(result.revision)
        // Activation flushes first, so it waits for the operation that applied these units.
        const reopened = reopenConversation
        if (reopened) void Promise.resolve().then(() => workingSet.activateConversation(reopened))
            .catch((error) => dependencies.onBackgroundError?.(error))
        else if (recovery) {
            const selected = dependencies.state.getSelectedCharacterId()
            const selectedCharacter = database.characters.find(value => value.chaId === selected)
            if (selected && selectedCharacter && result.affectedKeys.some(key => key === JSON.stringify(['archive', selected])) &&
                getCatalogCharacterMetadata(selectedCharacter)?.residency === 'catalog') {
                void Promise.resolve().then(() => workingSet.activateCharacter(selected)).catch(error => dependencies.onBackgroundError?.(error))
            }
        }
    }
    const fenceUnknownStoredReceive = () => {
        fenceUnknownPluginStorageDrain()
        dependencies.state.onPluginDeviceStorageChanged?.()
    }
    const fenceReceivedPluginStorage = (keys: readonly string[]) => {
        for (const unit of keys) {
            const [kind, owner, key] = JSON.parse(unit) as string[]
            if (kind === 'plugin') dependencies.state.onPluginStorageChanged?.(owner, key)
            else if (kind === 'plugin-local') dependencies.state.onPluginDeviceStorageChanged?.(owner)
        }
        advancePluginStorageRemoteChanges(keys)
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
            let result: LwwApplyResult
            try {
                result = await dependencies.store.lwwDrainDeferred!({ bindingAuthority: binding.targetAuthority, requestId: crypto.randomUUID(), generating: generating() })
            } catch (error) {
                fenceUnknownPluginStorageDrain()
                throw error
            }
            fenceReceivedPluginStorage(result.pluginStorageChangedKeys ?? result.affectedKeys)
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
                    const latestRevision = await readCurrentRevision(dependencies.store)
                    if (latestRevision !== token.revision || coordinator.revision !== token.revision) {
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
            const latestRevision = await readCurrentRevision(dependencies.store)
            const projected = await projectRefreshedWorkingSet(latestRevision, {selectedCharacterId:null, selectedConversationId:null, activeCharacterIds:new Set()}, {wholeLibrary:true, root:true, presets:true, pluginStorage:true, characterIds:[], conversations:[]})
            if (projected.deferred) throw new Error('Activated library projection is incomplete')
            coordinator.assertActivatedLibraryGuard(guard.owner, token)
            const rootBefore = canonicalClone(dependencies.state.captureRoot())
            workingSet.invalidateNavigation()
            dependencies.state.replaceDatabase(projected.database, new Set(), true)
            dependencies.state.restoreSelection?.(null, null)
            dependencies.state.afterRemoteApply?.()
            workingSet.installCommittedWorkingSet(dependencies.state.captureWorkingSetDatabase?.() ?? projected.database, latestRevision, projected.windowedMetadata)
            applyInstalledRootChanges(rootBefore)
            await commitContentCursor(latestRevision, true)
            guard.ready = true
            return {kind:'committed', revision:latestRevision, projection:'applied'}
        },
        async applyLwwReceive(request) {
            const staged = canonicalClone(request)
            return coordinator.withPausedPersistentWrites('lww-receive', async () => {
                const store = dependencies.store
                if (!store.lwwStageReceive || !store.lwwApplyReceive || !store.lwwFinishReceive) throw new Error('LWW receive is unavailable')
                const database = dependencies.state.captureWorkingSetDatabase?.()
                // An empty pull leaves the library alone; any key it still reports is projected against a lazy baseline.
                const baseline = database && staged.changes.length > 0 ? captureLwwWorkingSetBaseline(database, dependencies.state.captureRoot(), dependencies.state.capturePresets?.() ?? null, coordinator.captureMaterializedBaseline(), coordinator.capturePresetRecordBaseline()) : undefined
                await store.lwwStageReceive(staged)
                const header = { bindingAuthority: staged.bindingAuthority, requestId: staged.requestId }
                let result: LwwApplyResult
                try { result = await store.lwwApplyReceive({ ...header, generating: generating() }) }
                catch (error) {
                    fenceReceivedPluginStorage(staged.changes.map(change => change.key))
                    throw error
                }
                fenceReceivedPluginStorage(result.pluginStorageChangedKeys ?? result.affectedKeys)
                try { await projectAppliedUnits(result, baseline) }
                catch (error) { coordinator.markCommittedWorkingSetRefreshRequired(result.revision, error); throw error }
                finally { await store.lwwFinishReceive(header) }
                return result
            })
        },
        async applyStoredLwwReceive(request, apply) {
            const stored = canonicalClone(request)
            return coordinator.withPausedPersistentWrites('lww-receive', async (token) => {
                const store = dependencies.store
                if (!store.lwwFinishReceive) throw new Error('LWW receive is unavailable')
                const database = dependencies.state.captureWorkingSetDatabase?.()
                let baseline = database ? captureLwwWorkingSetBaseline(database, dependencies.state.captureRoot(), dependencies.state.capturePresets?.() ?? null, coordinator.captureMaterializedBaseline(), coordinator.capturePresetRecordBaseline()) : undefined
                const header = { bindingAuthority: stored.bindingAuthority, requestId: stored.requestId }
                let journalBaselineLease: PersistentRevisionLease | undefined = await store.acquireRevision(token.revision)
                let nativeBaseline: import('./persistentDataStore').NativeReceiveEvidence | undefined
                try { nativeBaseline = await store.readNativeReceiveEvidence?.(journalBaselineLease) }
                catch (error) { await releasePersistentRevisionLease(journalBaselineLease); throw error }
                const retiredJournalLeases = new Set<PersistentRevisionLease>()
                const releaseRetiredJournalLeases = async () => {
                    for (const lease of retiredJournalLeases) {
                        await releasePersistentRevisionLease(lease)
                        retiredJournalLeases.delete(lease)
                    }
                }
                const releaseJournalLeases = async () => {
                    await releaseRetiredJournalLeases()
                    if (journalBaselineLease) {
                        await releasePersistentRevisionLease(journalBaselineLease)
                        journalBaselineLease = undefined
                    }
                }
                const nativeRequest = { ...header, generating: generating(), ...(nativeBaseline ? { nativeBaseline } : {}) }
                let possiblyCommitted = false
                let coordinatorBaselineCoherent = true
                const complete = async (recoveryOwner?: symbol, capturedDiscard?: CapturedReceiveConversationEdits) => {
                    const discard = capturedDiscard && baseline ? {
                        ...prepareLwwReceiveDiscardBaseline(baseline, capturedDiscard),
                        captured: capturedDiscard, publication: capturedDiscard.prepareDiscard(),
                    } : undefined
                    const projectionBaseline = discard?.baseline ?? baseline
                    await releaseRetiredJournalLeases()
                    let result: LwwApplyResult
                    try { result = await apply(nativeRequest) }
                    catch (error) {
                        if (!possiblyCommitted && error && typeof error === 'object' && 'receiveNotApplied' in error && error.receiveNotApplied === true) throw error
                        possiblyCommitted = true
                        fenceUnknownStoredReceive()
                        result = await apply(nativeRequest)
                    }
                    possiblyCommitted = true
                    const projected = lastStoredReceiveProjection?.bindingAuthority === header.bindingAuthority && lastStoredReceiveProjection.requestId === header.requestId
                    const receivedKeys = projected ? [] : result.pluginStorageChangedKeys ?? result.affectedKeys
                    fenceReceivedPluginStorage(receivedKeys)
                    const fencedKeys = new Set(receivedKeys)
                    if (result.currentStateRecoveryRequired) fenceUnknownStoredReceive()
                    let revision = result.revision
                    let lease: PersistentRevisionLease
                    while (true) {
                        let candidate: PersistentRevisionLease | undefined
                        try {
                            candidate = await store.acquireRevision(revision)
                            const evidence = await store.readNativeReceiveEvidence?.(candidate)
                            if (evidence && nativeBaseline) {
                                const before = nativeBaseline.identity, current = evidence.identity
                                if (before.storeId !== current.storeId || before.libraryEpoch !== current.libraryEpoch ||
                                    before.generation !== current.generation || before.selectionEpoch !== current.selectionEpoch ||
                                    evidence.bindingAuthority !== header.bindingAuthority) {
                                    throw new ConversationJournalConflictError('receive-authority-changed')
                                }
                                const expected = result.nativeDevicePluginFence
                                if (!expected || expected.instanceId !== evidence.devicePlugins.instanceId || expected.revision !== evidence.devicePlugins.revision) {
                                    dependencies.state.onPluginDeviceStorageChanged?.()
                                }
                            }
                            lease = candidate
                            break
                        } catch (error) {
                            if (candidate) await releasePersistentRevisionLease(candidate)
                            if (!(error instanceof RevisionConflictError) || error.actualRevision === revision) throw error
                            fenceUnknownStoredReceive()
                            revision = error.actualRevision
                        }
                    }
                    try {
                        const rebased = (result.currentStateRecoveryRequired || revision !== result.revision || revision !== token.revision) && database && projectionBaseline
                            ? await readLwwReceiveRebaseKeys(database, projectionBaseline, lease, token.revision, result.currentStateRecoveryRequired)
                            : null
                        if (rebased?.unknownPluginChanges) fenceUnknownStoredReceive()
                        const keys = [...new Set([...(projected ? [] : result.affectedKeys), ...(rebased?.keys ?? []), ...(discard?.keys ?? [])])]
                        fenceReceivedPluginStorage((rebased?.keys ?? []).filter(key => !fencedKeys.has(key)))
                        for (let attempt = 0; ; attempt++) {
                            try {
                                await projectAppliedUnits({ ...result, revision, affectedKeys: keys }, projectionBaseline, false, rebased !== null, undefined,
                                    { owner: recoveryOwner, lease, baselineLease: journalBaselineLease, allowLocalFields: rebased !== null || discard !== undefined, discard,
                                        journalPublished: () => {
                                            coordinatorBaselineCoherent = false
                                            if (discard) baseline = discard.baseline
                                            if (journalBaselineLease && journalBaselineLease !== lease) retiredJournalLeases.add(journalBaselineLease)
                                            journalBaselineLease = lease
                                        }, coordinatorPublished: () => { coordinatorBaselineCoherent = true } })
                                break
                            } catch (error) {
                                if (discard || !(error instanceof ConversationJournalConflictError) || error.reason !== 'journal-changed' || attempt >= 7) throw error
                            }
                        }
                        lastStoredReceiveProjection = header
                    } finally {
                        try {
                            if (lease !== journalBaselineLease) await releasePersistentRevisionLease(lease)
                            await releaseRetiredJournalLeases()
                        } finally { await store.lwwFinishReceive!(header) }
                    }
                    return result
                }
                try {
                    const result = await complete()
                    await releaseJournalLeases()
                    return result
                } catch (error) {
                    if (!possiblyCommitted) { await releaseJournalLeases(); throw error }
                    fenceUnknownStoredReceive()
                    const recover = async (discard?: CapturedReceiveConversationEdits): Promise<CommittedApplyOutcome> => {
                        if (discard && !discard.isCurrent()) throw new ConversationJournalConflictError('journal-changed')
                        const owner = await coordinator.acquireCommittedWorkingSetRefreshFence()
                        try {
                            await complete(owner, discard)
                            await releaseJournalLeases()
                            coordinator.completeCommittedUnitProjection(owner)
                            pendingStoredReceiveRecovery = null
                            return { kind: 'committed', revision: coordinator.revision, projection: 'applied' }
                        } catch (error) { return requireCommittedRefresh(coordinator.revision, error) }
                        finally { coordinator.releaseDestructiveReplacementFence(owner) }
                    }
                    const recovery = {
                        retry: () => recover(),
                        captureDiscard: (characterId: string, conversationId: string): PersistentReceiveDiscardAction => {
                            if (!coordinatorBaselineCoherent) throw new ConversationJournalConflictError('journal-owner-changed')
                            const captured = coordinator.captureReceiveConversationEdits(characterId, conversationId, coordinator.revision)
                            return {
                                characterId, conversationId,
                                discard: () => {
                                    if (pendingStoredReceiveRecovery !== recovery || committedWorkingSetRecovery || !coordinatorBaselineCoherent || !captured.isCurrent()) {
                                        return Promise.reject(new ConversationJournalConflictError('journal-changed'))
                                    }
                                    const promise = recover(captured)
                                    committedWorkingSetRecovery = promise
                                    const clear = () => { if (committedWorkingSetRecovery === promise) committedWorkingSetRecovery = null }
                                    void promise.then(clear, clear)
                                    return promise
                                },
                            }
                        },
                    }
                    pendingStoredReceiveRecovery = recovery
                    coordinator.markCommittedWorkingSetRefreshRequired(coordinator.revision, error)
                    throw error
                }
            })
        },
        captureStoredReceiveDiscard: (characterId, conversationId) => pendingStoredReceiveRecovery?.captureDiscard(characterId, conversationId) ?? null,
        drainLwwDeferred,
        async commitPersistentUnitIntent(reason, mutations, conversations, wholeMessages, projectionObserver, check) {
            // Translation reads the effective identity records, so unsaved mirror
            // edits reach them first.
            dependencies.state.beforeCapture?.()
            const translated = translatePersistentRootUnitIntents(mutations)
            const affectedKeys = unitIntentAffectedKeys(translated, conversations ?? [])
            for (const value of wholeMessages ?? []) affectedKeys.push(JSON.stringify(['messages', value.characterId, value.conversationId]))
            await coordinator.commitPersistentUnitIntent(reason, translated, conversations, wholeMessages, async (revision) => {
                try { await projectAppliedUnits({ revision, affectedKeys, heldKeys: [], deferredKeys: [] }, undefined, true, false, projectionObserver) }
                catch (error) { coordinator.markCommittedWorkingSetRefreshRequired(revision, error); throw error }
            }, check)
        },
        async commitPreparedUnitIntent(reason, prepare) {
            dependencies.state.beforeCapture?.()
            let affectedKeys: string[] = []
            return coordinator.commitPreparedUnitIntent(reason, async (revision) => {
                const lease = await dependencies.store.acquireRevision(revision)
                let prepared: PreparedUnitIntent | null
                try { prepared = await prepare(lease) }
                finally { await releasePersistentRevisionLease(lease) }
                if (!prepared) return null
                const translated = translatePersistentRootUnitIntents(prepared.unitMutations)
                affectedKeys = unitIntentAffectedKeys(translated, prepared.conversations)
                return { unitMutations: translated, conversations: prepared.conversations }
            }, async (revision) => {
                try { await projectAppliedUnits({ revision, affectedKeys, heldKeys: [], deferredKeys: [] }, undefined, true, true) }
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
            // The loaded working set must not overwrite what a ticket that outlived the old one commits.
            const reinstalled = await reinstallNativeGenerations()
            await drainLwwDeferred()
            // A recreated WebView starts from a projection of the current
            // revision, so the cursor is realigned with it.
            await commitContentCursor(coordinator.revision)
            // Without native's list a surviving ticket is unguarded, so editing waits for a refresh retry.
            if (!reinstalled) coordinator.markCommittedWorkingSetRefreshRequired(coordinator.revision, nativeReinstallFailure?.error)
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
                if (pendingStoredReceiveRecovery) return pendingStoredReceiveRecovery.retry()
                if (coordinator.pendingWorkingSetRefreshRevision === null) return null
                // Tickets native may still commit for are guarded again before the working set reloads.
                if (nativeReinstallFailure && !await reinstallNativeGenerations()) {
                    return requireCommittedRefresh(coordinator.revision, nativeReinstallFailure?.error)
                }
                // A fenced native adoption keeps the edits it held until adopting again clears it. A
                // ticket that cannot be adopted stops guarding, and its target reloads below.
                const adopted = nativeRecoveries.size > 0 ? await retryNativeAdoptions() : null
                if (adopted && adopted !== 'reload') return adopted
                const revision = coordinator.pendingWorkingSetRefreshRevision ?? (adopted ? coordinator.revision : null)
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
        recordSelectedCharacterLastInteraction: (authority, before, after) =>
            coordinator.recordSelectedCharacterLastInteraction(authority, before, after),
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
            options?.signal?.throwIfAborted()
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
                if (!(await confirmUpstreamLibraryReplacement(binding.bound, options.upstreamImportWarnings, options))) throw new DOMException('Import cancelled', 'AbortError')
                options.signal?.throwIfAborted()
                const plugins = await import('../plugins/apiV3/v3.svelte')
                let pause: Awaited<ReturnType<typeof acquireUpstreamImportPause>>
                let pluginsFenced = false
                try {
                    await binding.fence()
                    pluginsFenced = true
                    await plugins.fencePluginExecutionForAuthorityReplacement()
                    pause = await acquireUpstreamImportPause(runtime, reason)
                } catch (error) {
                    try {
                        if (pluginsFenced) await plugins.restartPluginsAfterAuthorityReplacement()
                        await binding.assertAuthority()
                        await binding.resume()
                    } catch {} finally { binding.release() }
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
                    options.signal?.throwIfAborted()
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
                            const unchangedRevision = await readCurrentRevision(dependencies.store)
                            if (unchangedRevision !== pause.fence.revision || coordinator.revision !== pause.fence.revision) throw new PersistentMutationFencedError()
                            await plugins.restartPluginsAfterAuthorityReplacement()
                            await pause.abortUnchanged(async () => { await binding.assertAuthority() })
                            abortedUnchanged = true
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
        mutatePersistentPluginStorage: (reason, mutations, check) =>
            coordinator.mutatePersistentPluginStorage(reason, mutations, check),
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
        expirePersistentTrash: (now) => coordinator.expirePersistentTrash(now),
        mutatePersistentCharacterDetail: (characterId, reason, mutate) =>
            withCompleteCharacter(characterId, () => coordinator.mutatePersistentCharacterDetail(
                characterId,
                reason,
                mutate,
            )),
        deletePersistentCharacter: (characterId, reason) =>
            withCompleteCharacter(characterId, () => coordinator.deletePersistentCharacter(
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
            let releasing: Promise<void> | undefined
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
                release(options) {
                    if (releasing) return releasing
                    if (released) return
                    const finish = () => {
                        coordinator.releaseDestructiveReplacementFence(owner)
                        released = true
                        // Nothing else can commit while the fence is held, so a later
                        // revision is the holder's own change and sync must send it.
                        const committed = coordinator.revision
                        if (committed <= heldRevision) return
                        try {
                            dependencies.onLocalRevision?.(committed)
                        } catch (error) {
                            dependencies.onBackgroundError?.(error)
                        }
                    }
                    if (!options?.verifyStoreRevision || coordinator.revision > heldRevision) return finish()
                    releasing = (async () => {
                        try {
                            const latestRevision = await readCurrentRevision(dependencies.store)
                            if (latestRevision > coordinator.revision) coordinator.markCommittedWorkingSetRefreshRequired(
                                latestRevision, new Error('Native commit completed without a working-set acknowledgement'))
                        } catch (error) {
                            coordinator.markCommittedWorkingSetRefreshRequired(coordinator.revision, error)
                        } finally { finish() }
                    })()
                    return releasing
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
                    prepareRoot: dependencies.prepareRoot,
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
        assertNativeGenerationPreparationCurrent: (token) => coordinator.assertNativeGenerationPreparationCurrent(token),
        installNativeGenerationGuard(ticket, target, effectKeys = []) {
            nativeGuard.install(ticket, target, effectKeys)
            recordNativeTicket(ticket, target)
        },
        setNativeGenerationEffectKeys: (ticket, effectKeys) => nativeGuard.setEffectKeys(ticket, effectKeys),
        settleNativeGeneration: (status) => trackNative(nativeGuard.settle(status)),
        adoptNativeGenerationCommit: (proof) => coordinator.enqueueWithoutFlush(() => adoptNativeCommit(proof, {})),
        async adoptNativeGenerationCommitInQueue(token, proof) {
            coordinator.assertPausedPersistentWrites(token)
            await adoptNativeCommit(proof, { token })
        },
        closeNativeGenerations: (boundMs) => trackNative(nativeGuard.close(boundMs)),
        getNativeGenerationGuards: () => nativeGuard.snapshot(),
        publishCurrentOfficialRevision: () =>
            coordinator.publishCurrentOfficialRevision(),
        hasPendingOfficialPublication: () =>
            coordinator.hasPendingOfficialPublication,
    }
    return runtime
}
