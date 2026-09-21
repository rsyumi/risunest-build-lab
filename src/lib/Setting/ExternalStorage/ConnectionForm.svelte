<script lang="ts">
    import { onDestroy, onMount, tick } from 'svelte'
    import TextInput from 'src/lib/UI/GUI/TextInput.svelte'
    import SelectInput from 'src/lib/UI/GUI/SelectInput.svelte'
    import OptionInput from 'src/lib/UI/GUI/OptionInput.svelte'
    import SegmentedButtons from '../RisuNest/SegmentedButtons.svelte'
    import SettingButton from '../RisuNest/SettingButton.svelte'
    import SettingToggle from '../RisuNest/SettingToggle.svelte'
    import ExternalFolderSelector from './ExternalFolderSelector.svelte'
    import { openUrl } from '@tauri-apps/plugin-opener'
    import { type as osType } from '@tauri-apps/plugin-os'
    import { isTauriAndroid, isTauriIOS } from 'src/ts/platform'
    import { getExternalStorageBridge } from 'src/ts/storage/sync/external/bridge'
    import {
        buildPrepareConnectionRequest,
        defaultExternalCapturePolicy,
        requiredConnectionAcknowledgements,
    } from 'src/ts/storage/sync/external/connection'
    import {
        buildProviderSecret,
        externalProviderDefinitions,
        getExternalProviderDefinition,
    } from 'src/ts/storage/sync/external/providerRegistry'
    import type {
        ExternalConnectionResult,
        ExternalConnectionPurpose,
        ExternalOpenMode,
        ExternalProviderId,
        ExternalProviderDescriptor,
        PreparedExternalConnection,
        ExternalFolderSelection,
    } from 'src/ts/storage/sync/external/types'
    import {
        externalEndpointWarning,
        externalErrorMessage,
        externalErrorKind,
        externalFolderErrorKind,
        externalFieldHelp,
        externalFieldLabel,
        externalOptionLabel,
        externalProfileLabel,
        externalProviderName,
        type ExternalStorageStrings,
    } from './strings'

    interface Props {
        strings: ExternalStorageStrings
        onconnected: (result: ExternalConnectionResult) => void | Promise<void>
        oncancel: () => void
        onbusychange?: (busy: boolean) => void
        /** Opens an already existing repository only, without the create form. */
        restoreOnly?: boolean
        tone?: 'settings' | 'onboarding'
    }

    let {
        strings,
        onconnected,
        oncancel,
        onbusychange = () => {},
        restoreOnly = false,
        tone = 'settings',
    }: Props = $props()
    const bridge = getExternalStorageBridge()
    const FOLDER_NAME_ID = 'external-storage-folder-name'
    const platform = isTauriAndroid
        ? 'android'
        : isTauriIOS
          ? 'ios'
          : ({ windows: 'windows', macos: 'macos', linux: 'linux' } as Record<string, string>)[osType()] ?? 'windows'
    let providerId = $state<ExternalProviderId>('google_drive')
    let chosenMode = $state<ExternalOpenMode>('create')
    const mode = $derived<ExternalOpenMode>(restoreOnly ? 'existing' : chosenMode)
    let fromTransfer = $state(false)
    let purpose = $state<ExternalConnectionPurpose>('backup')
    let values = $state<Record<string, string>>({
        space: 'drive', accountType: 'personal', tenant: 'common', folderName: 'RisuNest',
        ...(isTauriAndroid ? {
            oauthRedirectUri: 'https://update.rsyumi.workers.dev/oauth/google-drive-callback',
        } : {}),
    })
    let hypa = $state(true)
    let localPlugins = $state(true)
    let localSettings = $state(true)
    let accepted = $state<string[]>([])
    let prepared = $state<PreparedExternalConnection | null>(null)
    let endpointConfirmed = $state(false)
    let connectionSettingsPayload = $state('')
    let recoveryKey = $state('')
    let pendingAuthorizationId = $state<string | null>(null)
    let currentPlatformClientId = $state('')
    let oauthClientSecret = $state('')
    let manualOAuthCallback = $state('')
    let providerDescriptors = $state<ExternalProviderDescriptor[]>([])
    let busy = $state(false)
    let error = $state('')
    let authorizationStatus = $state('')
    let destroyed = false
    let authorizationCompletionInFlight = false
    let cancellationId: string | null = null
    let cancellationPromise: Promise<boolean> | null = null
    let folder = $state<ExternalFolderSelection | null>(null)
    let folderError = $state('')
    let folderNameError = $state('')
    let selectingFolder = $state(false)
    let reselectRequired = $state(false)
    let folderSelector = $state<{ selectionId: string; accountHint?: string } | null>(null)
    let folderRow = $state<HTMLElement | undefined>()
    let previousFolder: ExternalFolderSelection | null = null

    const definition = $derived(getExternalProviderDefinition(providerId))
    const requiredAcks = $derived(requiredConnectionAcknowledgements(providerId))
    const googleOAuth = $derived(providerId === 'google_drive')
    const googleAndroid = $derived(isTauriAndroid && providerId === 'google_drive')
    const visibleLocation = $derived(
        providerId === 'google_drive' ? (values.space || 'drive') === 'drive'
        : providerId === 'onedrive' ? (values.accountType || 'personal') !== 'appFolder'
        : true,
    )
    const visibleFields = $derived(definition.fields.filter(field => (
        (field.key !== 'oauthRedirectUri' || googleAndroid)
        && (!field.createOnly || (mode === 'create' && visibleLocation))
    )))
    const folderNameField = $derived(visibleFields.find(field => field.createOnly) ?? null)
    const locationHelp = $derived(
        mode === 'existing' && visibleLocation && definition.fields.some(field => field.createOnly)
            ? strings.folderSelectHelp
            : '',
    )
    const folderSelection = $derived(prepared !== null && (prepared.requiresFolderSelection || reselectRequired))
    const folderRowState = $derived<'empty' | 'selecting' | 'selected' | 'invalid'>(
        selectingFolder ? 'selecting' : folder ? 'selected' : folderError ? 'invalid' : 'empty',
    )
    const accountHint = $derived(folder?.accountHint ?? folderSelector?.accountHint ?? prepared?.endpoint.accountHint)
    const authorizationAvailable = $derived(
        providerDescriptors.find(provider => provider.id === providerId)?.authorizationAvailable ?? true,
    )
    const providerStrings = $derived(strings.providers[providerId])
    const supportsSync = $derived(definition.supportsSync)
    const providerOptions = $derived(externalProviderDefinitions.map(provider => ({
        value: provider.id,
        label: externalProviderName(strings, provider.id),
        disabled: providerDescriptors.find(item => item.id === provider.id)?.authorizationAvailable === false,
    })))
    const purposeOptions = $derived([
        { value: 'backup' as const, label: strings.backup },
        ...(supportsSync ? [{ value: 'sync' as const, label: strings.sync }] : []),
    ])
    const scopeSummary = $derived([
        strings.library,
        ...(hypa ? [strings.hypa] : []),
        ...(localPlugins ? [strings.devicePlugins] : []),
        ...(localSettings ? [strings.deviceSettings] : []),
    ].join(', '))
    const providerWarning = $derived('warningTitle' in providerStrings
        ? { title: providerStrings.warningTitle, body: providerStrings.warning }
        : null)
    const connectLabel = $derived(prepared?.requiresOAuth && !folderSelection
        ? (pendingAuthorizationId ? strings.finishSignIn : strings.signIn)
        : strings.connect)

    $effect(() => onbusychange(busy))

    onMount(async () => {
        try {
            providerDescriptors = await bridge.listProviders()
        } catch (reason) {
            error = externalErrorMessage(strings, reason)
        }
    })
    onDestroy(() => {
        destroyed = true
        const authorizationId = pendingAuthorizationId
        if (authorizationId && !authorizationCompletionInFlight) {
            void cancelNativeAuthorization(authorizationId)
        }
        if (folderSelector) void bridge.cancelFolderSelection(folderSelector.selectionId).catch(() => {})
        onbusychange(false)
    })

    function cancelNativeAuthorization(authorizationId: string): Promise<boolean> {
        if (cancellationId === authorizationId && cancellationPromise) return cancellationPromise
        cancellationId = authorizationId
        cancellationPromise = bridge.cancelAuthorization(authorizationId)
            .then(() => true)
            .catch(() => false)
            .finally(() => {
                if (cancellationId === authorizationId) {
                    cancellationId = null
                    cancellationPromise = null
                }
            })
        return cancellationPromise
    }

    async function cancelPendingAuthorization(): Promise<boolean> {
        if (!pendingAuthorizationId) return true
        const authorizationId = pendingAuthorizationId
        if (await cancelNativeAuthorization(authorizationId)) {
            if (pendingAuthorizationId !== authorizationId) return true
            pendingAuthorizationId = null
            manualOAuthCallback = ''
            authorizationStatus = ''
            return true
        }
        error = strings.errorGeneric
        return false
    }

    async function resetPrepared(): Promise<void> {
        if (busy) return
        busy = true
        if (!(await cancelPendingAuthorization())) {
            busy = false
            return
        }
        clearPrepared()
        busy = false
    }

    function clearPrepared(): void {
        prepared = null
        fromTransfer = false
        endpointConfirmed = false
        currentPlatformClientId = ''
        oauthClientSecret = ''
        manualOAuthCallback = ''
        authorizationStatus = ''
        error = ''
        clearFolderSelection()
    }

    function clearFolderSelection(): void {
        if (folderSelector) void bridge.cancelFolderSelection(folderSelector.selectionId).catch(() => {})
        folderSelector = null
        folder = null
        previousFolder = null
        folderError = ''
        selectingFolder = false
        reselectRequired = false
    }

    function selectProvider(value: string): void {
        providerId = value as ExternalProviderId
        const next = getExternalProviderDefinition(providerId)
        if (!next.supportsSync) purpose = 'backup'
        values = {
            space: 'drive', accountType: 'personal', tenant: 'common', folderName: 'RisuNest',
            ...(isTauriAndroid ? {
                oauthRedirectUri: 'https://update.rsyumi.workers.dev/oauth/google-drive-callback',
            } : {}),
            uploadEndpoint: 'https://uploads.github.com',
            profile: next.profiles[0]?.value ?? '',
        }
        accepted = []
        folderNameError = ''
        resetPrepared()
    }

    function selectMode(value: ExternalOpenMode): void {
        chosenMode = value
        connectionSettingsPayload = ''
        recoveryKey = ''
        resetPrepared()
    }

    function readConnectionSettingsFile(): Promise<string | null> {
        return new Promise(resolve => {
            const input = document.createElement('input')
            input.type = 'file'
            // The Android picker hides extensions it does not know, so it stays
            // open there and the contents are checked when the key is read.
            if (!isTauriAndroid) input.accept = '.rnconnection,text/plain'
            input.style.display = 'none'
            const finish = async (file?: File) => {
                input.remove()
                resolve(file ? await file.text() : null)
            }
            input.addEventListener('cancel', () => void finish())
            input.addEventListener('change', () => void finish(input.files?.[0] ?? undefined))
            document.body.appendChild(input)
            input.click()
        })
    }

    async function loadConnectionSettingsFile(): Promise<void> {
        const contents = await readConnectionSettingsFile()
        if (contents === null) return
        connectionSettingsPayload = contents.replace(/^﻿/, '').trim()
        error = ''
    }

    async function scanConnectionSettings(): Promise<void> {
        try {
            const { checkPermissions, requestPermissions, scan, Format } = await import('@tauri-apps/plugin-barcode-scanner')
            let permission = await checkPermissions()
            if (permission === 'prompt') permission = await requestPermissions()
            if (permission !== 'granted') throw new Error('qr-camera-permission-denied')
            const result = await scan({ formats: [Format.QRCode], windowed: true, cameraDirection: 'back' })
            if (result.format !== Format.QRCode) throw new Error('invalid-connection-settings')
            connectionSettingsPayload = result.content.trim()
            error = ''
        } catch (reason) {
            error = externalErrorMessage(strings, reason)
        }
    }

    function selectPurpose(value: ExternalConnectionPurpose): void {
        purpose = value
        resetPrepared()
    }

    function fieldLabel(key: string): string {
        if (key === 'clientId' && googleAndroid) return strings.webOAuthClientId
        if (key === 'clientId' && isTauriIOS) return strings.iosOAuthClientId
        if (key === 'oauthRedirectUri') return strings.oauthCallbackUrl
        return externalFieldLabel(strings, providerId, key)
    }

    function updateValue(key: string, value: string): void {
        values[key] = value
        if (key === 'folderName') folderNameError = ''
        resetPrepared()
    }

    function toggleAcknowledgement(id: string, checked: boolean): void {
        accepted = checked ? [...new Set([...accepted, id])] : accepted.filter(item => item !== id)
        resetPrepared()
    }

    function locationValues(): Record<string, string> {
        return { ...values, folderName: folderNameField ? (values.folderName ?? '').trim() : '' }
    }

    async function focusFolderName(): Promise<void> {
        await tick()
        document.getElementById(FOLDER_NAME_ID)?.focus()
    }

    async function prepare(): Promise<void> {
        if (folderNameField && !(values.folderName ?? '').trim()) {
            folderNameError = strings.folderNameRequired
            await focusFolderName()
            return
        }
        busy = true
        error = ''
        let request: ReturnType<typeof buildPrepareConnectionRequest>
        try {
            request = buildPrepareConnectionRequest({
                providerId, values: locationValues(), platform, mode, purpose,
                recoveryKey: mode === 'existing' ? recoveryKey.trim() : undefined,
                capturePolicy: purpose === 'backup'
                    ? { hypa, localPlugins, localSettings }
                    : defaultExternalCapturePolicy(purpose),
                acknowledgements: accepted,
            })
        } catch {
            error = strings.invalidConfiguration
            busy = false
            return
        }
        try {
            prepared = await bridge.prepareConnection(request)
        } catch (reason) {
            error = externalErrorMessage(strings, reason)
        } finally {
            busy = false
        }
    }

    async function importConnectionSettings(): Promise<void> {
        busy = true
        error = ''
        try {
            prepared = await bridge.prepareConnectionSettingsImport(connectionSettingsPayload.trim(), recoveryKey.trim())
            fromTransfer = true
            providerId = prepared.endpoint.providerId
        } catch (reason) {
            error = externalErrorMessage(strings, reason)
        } finally {
            busy = false
        }
    }

    async function focusFolderAction(): Promise<void> {
        await tick()
        if (busy) {
            setTimeout(() => folderRow?.querySelector<HTMLButtonElement>('[data-folder-action]')?.focus(), 0)
            return
        }
        folderRow?.querySelector<HTMLButtonElement>('[data-folder-action]')?.focus()
    }

    function commitFolder(selection: ExternalFolderSelection): void {
        folder = selection
        previousFolder = null
        folderError = ''
        selectingFolder = false
        endpointConfirmed = false
        void focusFolderAction()
    }

    function restorePreviousFolder(): void {
        folder = previousFolder
        previousFolder = null
        selectingFolder = false
        void focusFolderAction()
    }

    function failFolderSelection(reason: unknown): void {
        error = ''
        folder = null
        previousFolder = null
        selectingFolder = false
        endpointConfirmed = false
        folderError = externalErrorMessage(strings, reason)
        void focusFolderAction()
    }

    async function selectFolder(): Promise<void> {
        if (!prepared || busy) return
        busy = true
        error = ''
        folderError = ''
        previousFolder = folder
        folder = null
        endpointConfirmed = false
        selectingFolder = true
        try {
            const pending = await bridge.beginAuthorization(
                prepared.preparationId,
                prepared.requiresPlatformOAuthClient ? currentPlatformClientId.trim() : undefined,
            )
            if (destroyed) {
                await cancelNativeAuthorization(pending.authorizationId)
                return
            }
            pendingAuthorizationId = pending.authorizationId
            authorizationStatus = pending.state === 'complete' ? '' : strings.authorizationWaiting
            if (pending.authorizationUrl) {
                await openUrl(pending.authorizationUrl)
                return
            }
            if (pending.state !== 'complete') return
            busy = false
            await finishFolderSelection()
        } catch (reason) {
            await cancelPendingAuthorization()
            failFolderSelection(reason)
        } finally {
            busy = false
        }
    }

    async function finishFolderSelection(): Promise<void> {
        if (!pendingAuthorizationId || busy) return
        busy = true
        error = ''
        authorizationCompletionInFlight = true
        try {
            const outcome = await bridge.completeAuthorization(
                pendingAuthorizationId,
                manualOAuthCallback.trim() || undefined,
                oauthClientSecret || undefined,
            ).finally(() => authorizationCompletionInFlight = false)
            if (destroyed) {
                if ('authorizationPending' in outcome) await cancelPendingAuthorization()
                return
            }
            if ('authorizationPending' in outcome) {
                authorizationStatus = outcome.callbackRejected
                    ? strings.callbackRejected
                    : strings.authorizationWaiting
                return
            }
            pendingAuthorizationId = null
            manualOAuthCallback = ''
            authorizationStatus = ''
            if ('folderSelected' in outcome) {
                commitFolder(outcome.folder)
            } else if ('folderSelectionRequired' in outcome) {
                folderSelector = { selectionId: outcome.selectionId, accountHint: outcome.accountHint }
            } else if ('folderSelectionCancelled' in outcome) {
                restorePreviousFolder()
            } else {
                selectingFolder = false
                oauthClientSecret = ''
                await onconnected(outcome)
            }
        } catch (reason) {
            await cancelPendingAuthorization()
            failFolderSelection(reason)
        } finally {
            busy = false
        }
    }

    function onSelectorSelected(selection: ExternalFolderSelection): void {
        folderSelector = null
        commitFolder(selection)
    }

    function onSelectorCancel(): void {
        folderSelector = null
        restorePreviousFolder()
    }

    async function connect(): Promise<void> {
        if (!prepared || !endpointConfirmed) return
        if (folderSelection && !folder) return
        busy = true
        error = ''
        try {
            if (folderSelection) {
                const result = await bridge.commitConnection(prepared.preparationId)
                oauthClientSecret = ''
                await onconnected(result)
                return
            }
            if (prepared.requiresOAuth) {
                if (!pendingAuthorizationId) {
                    const pending = await bridge.beginAuthorization(
                        prepared.preparationId,
                        prepared.requiresPlatformOAuthClient
                            ? currentPlatformClientId.trim()
                            : undefined,
                    )
                    if (destroyed) {
                        await cancelNativeAuthorization(pending.authorizationId)
                        return
                    }
                    pendingAuthorizationId = pending.authorizationId
                    authorizationStatus = pending.state === 'complete'
                        ? ''
                        : strings.authorizationWaiting
                    if (pending.authorizationUrl) {
                        await openUrl(pending.authorizationUrl)
                        return
                    }
                    if (pending.state !== 'complete') return
                }
                authorizationCompletionInFlight = true
                const result = await bridge.completeAuthorization(
                    pendingAuthorizationId,
                    manualOAuthCallback.trim() || undefined,
                    oauthClientSecret || undefined,
                ).finally(() => authorizationCompletionInFlight = false)
                if (destroyed) {
                    if ('authorizationPending' in result) await cancelPendingAuthorization()
                    return
                }
                if ('authorizationPending' in result) {
                    authorizationStatus = result.callbackRejected
                        ? strings.callbackRejected
                        : strings.authorizationWaiting
                    return
                }
                oauthClientSecret = ''
                manualOAuthCallback = ''
                pendingAuthorizationId = null
                authorizationStatus = ''
                if ('folderSelected' in result) {
                    reselectRequired = true
                    commitFolder(result.folder)
                    return
                }
                if ('folderSelectionRequired' in result) {
                    reselectRequired = true
                    selectingFolder = true
                    folderSelector = { selectionId: result.selectionId, accountHint: result.accountHint }
                    return
                }
                if ('folderSelectionCancelled' in result) {
                    reselectRequired = true
                    restorePreviousFolder()
                    return
                }
                await onconnected(result)
                return
            }
            const secret = fromTransfer ? undefined : buildProviderSecret(providerId, values)
            if (!fromTransfer && !secret) throw new Error('This provider requires OAuth authorization.')
            await onconnected(await bridge.commitConnection(prepared.preparationId, secret))
            for (const field of definition.secretFields) values[field.key] = ''
        } catch (reason) {
            await cancelPendingAuthorization()
            if (externalFolderErrorKind(reason)) {
                reselectRequired = true
                failFolderSelection(reason)
                return
            }
            if (mode === 'create' && externalErrorKind(reason) === 'folderNameConflict') {
                clearPrepared()
                folderNameError = strings.folderNameConflict
                await focusFolderName()
                return
            }
            error = externalErrorMessage(strings, reason)
        } finally {
            busy = false
        }
    }
</script>

<fieldset disabled={busy} class="form" data-external-storage-connection-form data-tone={tone}>
    <fieldset disabled={prepared !== null} class="contents">
    <section class="sub">
        <h3 class="sub-title">{strings.provider}</h3>
        <div class="fields two">
            <label class="field">
                <span>{strings.provider}</span>
                <SelectInput value={providerId} className="w-full disabled:opacity-50" onchange={event => selectProvider(event.currentTarget.value)}>
                    {#each providerOptions as option (option.value)}<OptionInput value={option.value} disabled={option.disabled}>{option.label}</OptionInput>{/each}
                </SelectInput>
                <small>{providerStrings.description}</small>
            </label>
            {#if !restoreOnly}<div class="field">
                <span>{strings.mode}</span>
                <SegmentedButtons value={mode} label={strings.mode} role="radiogroup" disabled={prepared !== null} onchange={selectMode} options={[{ value: 'create', label: strings.create }, { value: 'existing', label: strings.existing }]} />
                {#if mode === 'existing'}<small>{strings.existingHelp}</small>{/if}
            </div>{/if}
        </div>
        {#if !authorizationAvailable}<p class="note danger"><span>{strings.authorizationUnavailable}</span></p>{/if}
        {#if googleAndroid}<p class="note"><span>{strings.googleAndroidSetup}</span></p>{/if}
        {#if isTauriAndroid && providerId === 'onedrive'}<p class="note"><span>{strings.oneDriveAndroidSetup}</span></p>{/if}
        {#if isTauriIOS && providerId === 'google_drive'}<p class="note"><span>{strings.googleIOSSetup}</span></p>{/if}
        {#if isTauriIOS && providerId === 'onedrive'}<p class="note"><span>{strings.oneDriveIOSSetup}</span></p>{/if}
    </section>

    {#if mode === 'create'}
    <section class="sub">
        <h3 class="sub-title">{strings.purpose}</h3>
        <div class="field">
            <SegmentedButtons value={purpose} label={strings.purpose} role="radiogroup" disabled={prepared !== null} onchange={selectPurpose} options={purposeOptions} />
            {#if !supportsSync}<small>{strings.backupOnlyProvider}</small>{/if}
            <small>{strings.purposeHelp}</small>
        </div>
        {#if purpose === 'sync'}
            <div class="field">
                <small>{strings.sequentialWarning}</small>
            </div>
        {/if}
    </section>
    {/if}

    {#if providerWarning}
        <div class="warning">
            <strong>{providerWarning.title}</strong>
            <p>{providerWarning.body}</p>
        </div>
    {/if}
    {#each requiredAcks as acknowledgement (acknowledgement)}
        <div class="warning">
            <strong>{strings.githubWarningTitle}</strong>
            <p>{strings.githubWarning}</p>
            <SettingToggle
                showLabel
                label={strings.acknowledge}
                checked={accepted.includes(acknowledgement)}
                onchange={checked => toggleAcknowledgement(acknowledgement, checked)}
            />
        </div>
    {/each}

    {#if mode === 'existing'}
    <section class="sub">
        <h3 class="sub-title">{strings.connectionSettings}</h3>
        <p class="sub-help">{strings.connectionSettingsImportHelp}</p>
        <div class="actions">
            <SettingButton variant="secondary" disabled={busy} onclick={loadConnectionSettingsFile}>{strings.openConnectionSettingsFile}</SettingButton>
            {#if isTauriAndroid || isTauriIOS}<SettingButton variant="secondary" disabled={busy} onclick={scanConnectionSettings}>{strings.scanConnectionSettings}</SettingButton>{/if}
        </div>
        <label class="field"><span>{strings.connectionSettingsPayload}</span><textarea class="textarea rounded-md border border-darkborderc bg-transparent px-4 py-2 text-textcolor shadow-xs transition-colors duration-200 focus:border-borderc focus:ring-2 focus:ring-borderc focus:outline-hidden disabled:opacity-50" placeholder={strings.connectionSettingsPayloadPlaceholder} bind:value={connectionSettingsPayload}></textarea></label>
        <label class="field"><span>{strings.recoveryCode}</span><TextInput className="disabled:opacity-50" fullwidth hideText bind:value={recoveryKey} /></label>
        <div class="actions">
            <SettingButton disabled={busy || !connectionSettingsPayload.trim() || !recoveryKey.trim()} onclick={importConnectionSettings}>{strings.importConnectionSettings}</SettingButton>
        </div>
        <p class="sub-help">{strings.manualConnectionHelp}</p>
    </section>
    {/if}

    <section class="sub">
        <h3 class="sub-title">{strings.connectionInfo}</h3>
        <div class="fields two">
            {#if definition.customEndpoint}
                <label class="field span2"><span>{strings.endpoint}</span><TextInput className="disabled:opacity-50" fullwidth value={values.endpoint ?? definition.defaultEndpoint} onchange={event => updateValue('endpoint', event.currentTarget.value)} placeholder={definition.defaultEndpoint || 'https://…'} /></label>
            {/if}
            {#if definition.profiles.length > 1}
                <label class="field"><span>{strings.profile}</span>
                    <SelectInput value={values.profile ?? definition.profiles[0].value} className="w-full disabled:opacity-50" onchange={event => updateValue('profile', event.currentTarget.value)}>
                        {#each definition.profiles as profile (profile.value)}<OptionInput value={profile.value}>{externalProfileLabel(strings, providerId, profile.value, profile.label)}</OptionInput>{/each}
                    </SelectInput>
                </label>
            {/if}
            {#each visibleFields as field (field.key)}
                {@const help = externalFieldHelp(strings, providerId, field.key)}
                <label class="field">
                    <span>{fieldLabel(field.key)}{field.required ? '' : strings.optional}</span>
                    {#if field.type === 'select'}
                        <SelectInput value={values[field.key] ?? field.options?.[0] ?? ''} className="w-full disabled:opacity-50" onchange={event => updateValue(field.key, event.currentTarget.value)}>
                            {#each field.options ?? [] as option (option)}<OptionInput value={option}>{externalOptionLabel(strings, providerId, field.key, option)}</OptionInput>{/each}
                        </SelectInput>
                    {:else if field.createOnly}
                        <TextInput id={FOLDER_NAME_ID} className="disabled:opacity-50" fullwidth value={values[field.key] ?? ''} oninput={() => folderNameError = ''} onchange={event => updateValue(field.key, event.currentTarget.value)} placeholder={field.placeholder ?? ''} />
                    {:else}
                        <TextInput className="disabled:opacity-50" fullwidth value={values[field.key] ?? ''} onchange={event => updateValue(field.key, event.currentTarget.value)} placeholder={field.placeholder ?? ''} />
                    {/if}
                    {#if field.createOnly && folderNameError}<small class="field-error" role="alert">{folderNameError}</small>{/if}
                    {#if help}<small>{help}</small>{/if}
                    {#if locationHelp && (field.key === 'space' || field.key === 'accountType')}<small>{locationHelp}</small>{/if}
                </label>
            {/each}
        </div>
    </section>

    {#if mode === 'create' && purpose === 'backup'}
    <fieldset class="sub">
        <legend class="sub-title">{strings.scope}</legend>
        <p class="note"><span>{strings.scopeHelp}</span></p>
        <p class="always"><span>{strings.library}</span><span class="value">{strings.included}</span></p>
        <p class="note"><span>{strings.libraryHelp}</span></p>
        <SettingToggle showLabel label={strings.hypa} bind:checked={hypa} onchange={resetPrepared} />
        <p class="note"><span>{strings.hypaHelp}</span></p>
        <SettingToggle showLabel label={strings.devicePlugins} bind:checked={localPlugins} onchange={resetPrepared} />
        <p class="note"><span>{strings.devicePluginsHelp}</span></p>
        <SettingToggle showLabel label={strings.deviceSettings} bind:checked={localSettings} onchange={resetPrepared} />
        <p class="note"><span>{strings.deviceSettingsHelp}</span></p>
    </fieldset>
    {/if}
    </fieldset>

    {#if prepared}
        <section class="sub">
            <h3 class="sub-title">{strings.endpointReview}</h3>
            <dl class="review">
                <dt>{strings.authority}</dt><dd>{prepared.endpoint.authority}</dd>
                {#if accountHint}<dt>{strings.account}</dt><dd>{accountHint}</dd>{/if}
                {#if folderSelection}
                    <dt>{strings.folder}</dt>
                    <dd class="folder" bind:this={folderRow} data-folder-row data-state={folderRowState}>
                        {#if folderRowState === 'selected' && folder}
                            <span class="folder-name">{folder.name}</span>
                            <SettingButton variant="secondary" data-folder-action disabled={!authorizationAvailable} onclick={selectFolder}>{strings.selectFolderAgain}</SettingButton>
                        {:else if folderRowState === 'selecting'}
                            <span class="folder-status" role="status">{strings.selectingFolder}</span>
                            {#if pendingAuthorizationId}<SettingButton variant="secondary" data-folder-action {busy} onclick={finishFolderSelection}>{strings.finishSignIn}</SettingButton>{/if}
                        {:else}
                            <SettingButton variant="secondary" data-folder-action disabled={!authorizationAvailable} onclick={selectFolder}>{strings.selectFolder}</SettingButton>
                        {/if}
                        {#if folderError}<span class="field-error" role="alert">{folderError}</span>{/if}
                    </dd>
                {:else}
                    <dt>{strings.repository}</dt><dd>{prepared.endpoint.repositoryHint}</dd>
                {/if}
                {#if mode === 'create'}
                    <dt>{strings.purposeReview}</dt>
                    <dd>{purpose === 'backup' ? `${strings.backup} · ${strings.includes.replace('{0}', scopeSummary)}` : strings.sync}</dd>
                {/if}
                {#if prepared.requiresPlatformOAuthClient && prepared.oauthProjectHint}<dt>{strings.oauthProjectHint}</dt><dd>{prepared.oauthProjectHint}</dd>{/if}
            </dl>
            {#if mode === 'create' && purpose === 'sync'}
                <p class="note"><span>{strings.syncPurposeLocalDataNotice}</span></p>
            {/if}
            {#each prepared.endpoint.warnings as warning (warning)}<p class="note"><span>{externalEndpointWarning(strings, warning)}</span></p>{/each}

            {#if folderSelection}
                {#if prepared.requiresPlatformOAuthClient}
                    <label class="field"><span>{googleAndroid ? strings.webOAuthClientId : strings.platformClientId}</span><TextInput className="disabled:opacity-50" fullwidth bind:value={currentPlatformClientId} /></label>
                {/if}
                {#if googleOAuth}
                    <div class="fields two">
                        <label class="field"><span>{strings.oauthClientSecret}</span><TextInput className="disabled:opacity-50" fullwidth hideText bind:value={oauthClientSecret} /></label>
                        {#if googleAndroid && pendingAuthorizationId}<label class="field"><span>{strings.manualOAuthCallback}</span><TextInput className="disabled:opacity-50" fullwidth hideText bind:value={manualOAuthCallback} /><small>{strings.manualOAuthHelp}</small></label>{/if}
                    </div>
                {/if}
                {#if authorizationStatus}<p class="sub-help" role="status">{authorizationStatus}</p>{/if}
                <SettingToggle showLabel label={strings.confirmEndpoint} disabled={!folder} bind:checked={endpointConfirmed} />
                <div class="actions">
                    <SettingButton {busy} disabled={!folder || !endpointConfirmed || selectingFolder || !authorizationAvailable} onclick={connect}>{strings.connect}</SettingButton>
                    <SettingButton variant="secondary" onclick={resetPrepared}>{strings.back}</SettingButton>
                </div>
            {:else}
            <SettingToggle showLabel label={strings.confirmEndpoint} bind:checked={endpointConfirmed} />
            {#if prepared.requiresPlatformOAuthClient}
                <label class="field"><span>{googleAndroid ? strings.webOAuthClientId : strings.platformClientId}</span><TextInput className="disabled:opacity-50" fullwidth bind:value={currentPlatformClientId} /></label>
            {/if}
            {#if endpointConfirmed && prepared.requiresOAuth && googleOAuth}
                <div class="fields two">
                    <label class="field"><span>{strings.oauthClientSecret}</span><TextInput className="disabled:opacity-50" fullwidth hideText bind:value={oauthClientSecret} /></label>
                    {#if googleAndroid && pendingAuthorizationId}<label class="field"><span>{strings.manualOAuthCallback}</span><TextInput className="disabled:opacity-50" fullwidth hideText bind:value={manualOAuthCallback} /><small>{strings.manualOAuthHelp}</small></label>{/if}
                </div>
            {/if}
            {#if endpointConfirmed && !prepared.requiresOAuth && !fromTransfer}
                <div class="fields two">
                    {#each definition.secretFields as field (field.key)}
                        {@const help = externalFieldHelp(strings, providerId, field.key)}
                        <label class="field">
                            <span>{fieldLabel(field.key)}</span>
                            {#if field.type === 'select'}
                                <SelectInput value={values[field.key] ?? field.options?.[0] ?? ''} className="w-full disabled:opacity-50" onchange={event => values[field.key] = event.currentTarget.value}>
                                    {#each field.options ?? [] as option (option)}<OptionInput value={option}>{externalOptionLabel(strings, providerId, field.key, option)}</OptionInput>{/each}
                                </SelectInput>
                            {:else if field.type === 'datetime-local'}
                                <input type="datetime-local" class="datetime rounded-md border border-darkborderc bg-transparent px-4 py-2 text-textcolor shadow-xs transition-colors duration-200 focus:border-borderc focus:ring-2 focus:ring-borderc focus:outline-hidden disabled:opacity-50" bind:value={values[field.key]} />
                            {:else}
                                <TextInput className="disabled:opacity-50" fullwidth hideText={field.secret} bind:value={values[field.key]} />
                            {/if}
                            {#if help}<small>{help}</small>{/if}
                        </label>
                    {/each}
                </div>
            {/if}
            {#if authorizationStatus}<p class="sub-help" role="status">{authorizationStatus}</p>{/if}
            <div class="actions">
                <SettingButton {busy} disabled={!endpointConfirmed || !authorizationAvailable || (prepared.requiresPlatformOAuthClient && !currentPlatformClientId.trim())} onclick={connect}>{connectLabel}</SettingButton>
                <SettingButton variant="secondary" onclick={resetPrepared}>{strings.back}</SettingButton>
            </div>
            {#if !prepared.requiresOAuth && mode === 'create'}<p class="sub-help">{strings.connectHint}</p>{/if}
            {/if}
        </section>
    {:else}
        <section class="sub">
            <div class="actions">
                <SettingButton {busy} disabled={!authorizationAvailable || (mode === 'existing' && !recoveryKey.trim()) || requiredAcks.some(item => !accepted.includes(item))} onclick={prepare}>{strings.prepare}</SettingButton>
                <SettingButton variant="secondary" onclick={oncancel}>{strings.cancel}</SettingButton>
            </div>
            <p class="sub-help">{strings.pendingVerification}</p>
        </section>
    {/if}
    {#if error}<p class="form-error" role="alert">{error}</p>{/if}
</fieldset>

{#if folderSelector}
    <ExternalFolderSelector {strings} selectionId={folderSelector.selectionId} onselected={onSelectorSelected} oncancel={onSelectorCancel} />
{/if}

<style>
    .form {
        display: grid;
        min-width: 0;
    }
    .form > .sub,
    .form > .contents > .sub + .sub {
        border-top: 1px solid color-mix(in srgb, var(--risu-theme-darkborderc) 55%, transparent);
    }
    /* The onboarding panel draws its own frame and gutter. */
    .form[data-tone='onboarding'] > .sub,
    .form[data-tone='onboarding'] > .contents > .sub + .sub {
        border-top: 0;
    }
    .form[data-tone='onboarding'] .sub {
        padding: 0 0 0.75rem;
    }
    .form[data-tone='onboarding'] .warning {
        margin: 0 0 0.75rem;
    }
    .form[data-tone='onboarding'] .form-error {
        padding: 0 0 0.75rem;
    }
    .sub {
        display: grid;
        gap: 0.75rem;
        padding: 0.9rem 1rem 1rem;
        min-width: 0;
    }
    .sub-title {
        margin: 0;
        padding: 0;
        font-size: 0.9375rem;
        font-weight: 600;
    }
    .fields {
        display: grid;
        gap: 0.75rem;
        grid-template-columns: minmax(0, 1fr);
    }
    @container (min-width: 40rem) {
        .fields.two {
            grid-template-columns: repeat(2, minmax(0, 1fr));
        }
        .fields.two .span2 {
            grid-column: 1 / -1;
        }
    }
    .field {
        display: grid;
        align-content: start;
        gap: 0.35rem;
        min-width: 0;
        font-size: 0.875rem;
    }
    .field > span {
        font-weight: 500;
    }
    .field > small,
    .sub-help {
        margin: 0;
        font-size: 0.8125rem;
        line-height: 1.45;
        color: var(--risu-theme-textcolor2);
    }
    .always {
        display: flex;
        flex-wrap: wrap;
        gap: 0.35rem;
        margin: 0;
        font-size: 0.8125rem;
        color: var(--risu-theme-textcolor2);
    }
    .always .value {
        font-weight: 600;
    }
    .actions {
        display: flex;
        flex-wrap: wrap;
        gap: 0.5rem;
    }
    .form-error {
        margin: 0;
        padding: 0 1rem 1rem;
        font-size: 0.875rem;
        color: var(--risu-theme-danger-400);
    }
    .note {
        display: grid;
        margin: 0;
        padding: 0.65rem 0.85rem;
        border: 1px solid var(--risu-theme-darkborderc);
        border-radius: 0.5rem;
        background: var(--risu-theme-bgcolor);
        font-size: 0.8125rem;
        line-height: 1.45;
        color: var(--risu-theme-textcolor2);
    }
    .note.danger {
        color: var(--risu-theme-danger-400);
        border-color: color-mix(in srgb, var(--risu-theme-danger-400) 45%, transparent);
    }
    .warning {
        display: grid;
        gap: 0.4rem;
        margin: 0 1rem 1rem;
        padding: 0.85rem 1rem;
        border: 1px solid color-mix(in srgb, var(--risu-theme-danger-400) 45%, transparent);
        border-left-width: 3px;
        border-radius: 0.5rem;
        background: color-mix(in srgb, var(--risu-theme-danger-400) 6%, transparent);
        font-size: 0.875rem;
    }
    .warning strong {
        font-weight: 600;
    }
    .warning p {
        margin: 0;
        font-size: 0.8125rem;
        line-height: 1.45;
        opacity: 0.85;
    }
    .review {
        display: grid;
        grid-template-columns: minmax(0, 1fr);
        gap: 0.15rem 1.1rem;
        margin: 0;
        padding: 0.85rem 1rem;
        border: 1px solid color-mix(in srgb, var(--risu-theme-primary-500) 35%, transparent);
        border-radius: 0.5rem;
        background: color-mix(in srgb, var(--risu-theme-primary-500) 8%, transparent);
        font-size: 0.85rem;
    }
    .review dt {
        font-size: 0.78rem;
        color: var(--risu-theme-textcolor2);
    }
    .review dd {
        margin: 0 0 0.5rem;
        min-width: 0;
        overflow-wrap: anywhere;
        font-weight: 600;
    }
    .review dd:last-child {
        margin-bottom: 0;
    }
    .review dd.folder {
        display: flex;
        flex-wrap: wrap;
        align-items: center;
        gap: 0.5rem 0.75rem;
        font-weight: 400;
    }
    .folder-name {
        min-width: 0;
        overflow-wrap: anywhere;
        font-weight: 600;
    }
    .folder-status {
        font-weight: 500;
        color: var(--risu-theme-textcolor2);
    }
    .field-error {
        flex-basis: 100%;
        margin: 0;
        font-size: 0.8125rem;
        line-height: 1.45;
        font-weight: 400;
    }
    .field > small.field-error,
    .review .field-error {
        color: var(--risu-theme-danger-400);
    }
    .textarea,
    .datetime {
        width: 100%;
        min-width: 0;
        font: inherit;
    }
    .textarea {
        min-height: 6rem;
        resize: vertical;
    }
</style>
