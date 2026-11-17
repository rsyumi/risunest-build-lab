import type { PersistentDataStore, PersistentRevisionLease } from './persistentDataStore'

type OptionalKey<T> = { [K in keyof T]-?: {} extends Pick<T, K> ? K : never }[keyof T]
type MemberKind<T> = {
    [K in keyof T]-?: K extends OptionalKey<T>
        ? 'optional'
        : T[K] extends (...args: never[]) => unknown ? 'method' : 'value'
}

// Every lease member, so a new member fails `pnpm check` here instead of in each fake.
const leaseMembers = {
    messageMetadataReadMode: 'optional',
    revision: 'value',
    readRoot: 'method',
    queryPresets: 'method',
    readPreset: 'method',
    queryCharacters: 'method',
    readCharacterSummary: 'method',
    readCharacter: 'method',
    queryConversations: 'method',
    readConversation: 'method',
    readConversationMetadata: 'method',
    readConversationWindow: 'method',
    readConversationMessageMetadataWindow: 'optional',
    readPluginStorageValues: 'method',
    queryPluginStorage: 'method',
    readPluginStorage: 'method',
    readAssetAlias: 'method',
    readAssetAliasesByKeys: 'method',
    listAssetAliases: 'method',
    readAssetOwnerHead: 'method',
    readWorkingSetChangeWindow: 'optional',
    readWorkingSetChangePage: 'optional',
    release: 'method',
} satisfies MemberKind<PersistentRevisionLease>

// Every store member, for the same reason.
const storeMembers = {
    messageMetadataReadMode: 'optional',
    lwwBindingState: 'optional',
    lwwReadOutbox: 'optional',
    lwwCommitReplacement: 'optional',
    lwwStageReceive: 'optional',
    lwwApplyReceive: 'optional',
    lwwFinishReceive: 'optional',
    lwwDrainDeferred: 'optional',
    open: 'method',
    readRoot: 'method',
    readRevision: 'optional',
    readLibraryIdentity: 'optional',
    queryPresets: 'method',
    readPreset: 'method',
    queryCharacters: 'method',
    readCharacterSummary: 'method',
    readCharacter: 'method',
    queryConversations: 'method',
    queryAuthorNotePresetUsage: 'optional',
    readConversation: 'method',
    readConversationMetadata: 'method',
    readConversationWindow: 'method',
    readConversationMessageMetadataWindow: 'optional',
    readPluginStorageValues: 'method',
    queryPluginStorage: 'method',
    readPluginStorage: 'method',
    listPluginStorage: 'method',
    commitWorkingSetChangeCursor: 'optional',
    readAssetAlias: 'method',
    readAssetAliasesByKeys: 'method',
    listAssetAliases: 'method',
    readAssetOwnerHead: 'method',
    commitAssetAlias: 'method',
    deleteAssetAlias: 'method',
    commit: 'method',
    archivePreview: 'method',
    archiveCharacter: 'method',
    restoreCharacter: 'method',
    stageDatabaseReplacement: 'optional',
    replaceFromDatabase: 'method',
    materializeDatabase: 'method',
    acquireRevision: 'method',
} satisfies MemberKind<PersistentDataStore>

function createStub<T, Extra extends object>(
    kinds: Record<string, 'method' | 'value' | 'optional'>,
    members: Partial<T>,
    extra: Extra,
): T & Extra {
    const stub: object = {}
    for (const [name, kind] of Object.entries(kinds)) {
        if (kind === 'optional' || Object.hasOwn(members, name)) continue
        if (kind === 'method') {
            Object.assign(stub, { [name]: () => { throw new Error(`unexpected call: ${name}`) } })
        } else {
            Object.defineProperty(stub, name, {
                configurable: true,
                get() { throw new Error(`unexpected read: ${name}`) },
            })
        }
    }
    Object.defineProperties(stub, Object.getOwnPropertyDescriptors(members))
    return Object.defineProperties(stub, Object.getOwnPropertyDescriptors(extra)) as T & Extra
}

/**
 * A revision lease built from `members`, plus `extra` own keys such as the native lease handle.
 * An omitted method throws `unexpected call: <name>`, an omitted `revision` throws when read,
 * and an omitted optional member stays absent, as on a store without that capability.
 */
export function createLeaseStub<Extra extends object = {}>(
    members: Partial<PersistentRevisionLease>,
    extra?: Extra,
): PersistentRevisionLease & Extra {
    return createStub<PersistentRevisionLease, Extra>(leaseMembers, members, extra ?? {} as Extra)
}

/**
 * A store built from `members`, plus `extra` own keys that the code under test reads beside the
 * store interface. An omitted method throws `unexpected call: <name>`, and an omitted optional
 * member stays absent, as on a store without that capability.
 */
export function createStoreStub<Extra extends object = {}>(
    members: Partial<PersistentDataStore>,
    extra?: Extra,
): PersistentDataStore & Extra {
    return createStub<PersistentDataStore, Extra>(storeMembers, members, extra ?? {} as Extra)
}
