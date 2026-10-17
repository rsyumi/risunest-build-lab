<script lang="ts">
    import { onDestroy, onMount } from 'svelte'
    import { language } from 'src/lang'
    import { alertMd } from 'src/ts/alertQueue'
    import { diagnosticInfo, formatDiagnosticLog } from 'src/ts/diagnostics'
    import { consoleLog, type ConsoleLogEntry } from 'src/ts/consoleLog'
    import SettingGroup from '../RisuNest/SettingGroup.svelte'
    import SettingRow from '../RisuNest/SettingRow.svelte'
    import SettingToggle from '../RisuNest/SettingToggle.svelte'
    import SettingButton from '../RisuNest/SettingButton.svelte'
    import {
        getNativeLogFilePath,
        getNativeLogTail,
        setNativeLogFileEnabled,
        type NativeLogEntry,
    } from 'src/ts/nativeLog'
    import {
        getDeviceSettings,
        subscribeDeviceSettings,
        updateDeviceSettings,
    } from 'src/ts/storage/deviceSettings'

    let entries = $state<NativeLogEntry[]>([])
    let logLoaded = $state(false)
    let errorMessage = $state('')
    let fileLogEnabled = $state(getDeviceSettings().nativeFileLogEnabled)
    let fileLogPath = $state('')
    let fileLogUpdatePending = $state(false)
    type LogKind = 'native' | 'console'
    let pending = $state<'view' | 'copy' | 'consoleView' | 'consoleCopy' | null>(null)
    let consoleRevision = 0
    let viewRequest = 0
    let copyRequest = 0
    let clipboardWriteQueue = Promise.resolve()

    function formatLog(logEntries: (NativeLogEntry | ConsoleLogEntry)[]) {
        return logEntries
        .slice()
        .reverse()
        .sort((left, right) => right.tsMs - left.tsMs)
        .map((entry) => `[${new Date(entry.tsMs).toISOString()}] [${entry.level}] ${'source' in entry ? `[${entry.source}] ` : ''}${entry.message}`)
        .join('\n')
    }

    /**
     * Log lines are shown through a markdown renderer, so they have to be fenced
     * to stay verbatim. The fence is a run of tildes that is always longer than
     * any tilde run inside the log itself, so log content can never close it.
     */
    function fenceLog(text: string) {
        let longestTildeRun = 0
        for (const match of text.matchAll(/~+/g)) longestTildeRun = Math.max(longestTildeRun, match[0].length)
        const fence = '~'.repeat(Math.max(4, longestTildeRun + 1))
        return `${fence}\n${text}\n${fence}`
    }

    const unsubscribe = subscribeDeviceSettings((settings) => {
        fileLogEnabled = settings.nativeFileLogEnabled
        if (fileLogEnabled && !fileLogUpdatePending) void loadFilePath()
    })

    onDestroy(() => {
        unsubscribe()
        viewRequest++
        copyRequest++
    })

    onMount(() => {
        if (fileLogEnabled) void loadFilePath()
    })

    async function loadFilePath() {
        try {
            fileLogPath = await getNativeLogFilePath()
        } catch {
            errorMessage = language.risuNest.diag.actionFailed
        }
    }

    function readLog(kind: LogKind) {
        if (kind === 'console') return consoleLog.snapshot()
        return getNativeLogTail()
    }

    function emptyLog(kind: LogKind) {
        return kind === 'console' ? language.risuNest.diag.consoleEmpty : language.risuNest.diag.logEmpty
    }

    function clearConsole() {
        consoleLog.clear()
        consoleRevision++
        if (pending === 'consoleView' || pending === 'consoleCopy') pending = null
    }

    async function viewLog(kind: LogKind = 'native') {
        const request = ++viewRequest
        const revision = consoleRevision
        pending = kind === 'console' ? 'consoleView' : 'view'
        try {
            const freshEntries = await readLog(kind)
            if (request !== viewRequest || (kind === 'console' && revision !== consoleRevision)) return
            if (kind === 'native') {
                entries = freshEntries as NativeLogEntry[]
                logLoaded = true
            }
            errorMessage = ''
            const text = formatDiagnosticLog($diagnosticInfo, formatLog(freshEntries))
            alertMd(text ? fenceLog(text) : emptyLog(kind), true)
        } catch {
            if (request === viewRequest) errorMessage = language.risuNest.diag.actionFailed
        } finally {
            if (request === viewRequest) pending = null
        }
    }

    async function copyLog(kind: LogKind = 'native') {
        const request = ++copyRequest
        const revision = consoleRevision
        pending = kind === 'console' ? 'consoleCopy' : 'copy'
        try {
            const freshEntries = await readLog(kind)
            if (request !== copyRequest || (kind === 'console' && revision !== consoleRevision)) return
            if (kind === 'native') {
                entries = freshEntries as NativeLogEntry[]
                logLoaded = true
            }
            errorMessage = ''
            const text = formatDiagnosticLog($diagnosticInfo, formatLog(freshEntries)) || emptyLog(kind)
            // The button is free again once the log is read; a slow clipboard
            // write is serialized below, so a repeat request cannot reorder it.
            pending = null
            const pendingWrite = clipboardWriteQueue.then(async () => {
                if (request !== copyRequest || (kind === 'console' && revision !== consoleRevision)) return
                try {
                    await navigator.clipboard.writeText(text)
                } catch {
                    if (request !== copyRequest || (kind === 'console' && revision !== consoleRevision)) return
                    const textarea = document.createElement('textarea')
                    textarea.value = text
                    document.body.appendChild(textarea)
                    textarea.select()
                    try {
                        if (!document.execCommand('copy')) throw new Error('copy failed')
                    } finally {
                        document.body.removeChild(textarea)
                    }
                }
            })
            clipboardWriteQueue = pendingWrite.catch(() => undefined)
            await pendingWrite
        } catch {
            if (request === copyRequest) errorMessage = language.risuNest.diag.actionFailed
        } finally {
            if (request === copyRequest) pending = null
        }
    }

    async function changeFileLogging(enabled: boolean) {
        if (fileLogUpdatePending) return
        fileLogUpdatePending = true
        fileLogEnabled = enabled
        try {
            await setNativeLogFileEnabled(enabled)
            updateDeviceSettings({ nativeFileLogEnabled: enabled })
            fileLogEnabled = enabled
            if (enabled) await loadFilePath()
        } catch {
            fileLogEnabled = getDeviceSettings().nativeFileLogEnabled
            errorMessage = language.risuNest.diag.actionFailed
        } finally {
            fileLogUpdatePending = false
        }
    }
</script>

<SettingGroup id="risunest-diag" title={language.risuNest.diag.title}>
    <SettingRow label={language.risuNest.diag.logTitle} help={language.risuNest.diag.logHelp}>
        {#snippet below()}
            {#if errorMessage}
                <p class="mt-1 text-sm text-danger-400" role="alert" aria-live="assertive">{errorMessage}</p>
            {:else if logLoaded && entries.length === 0}
                <p class="mt-1 text-sm text-textcolor2" role="status" aria-live="polite">{language.risuNest.diag.logEmpty}</p>
            {/if}
        {/snippet}
        <SettingButton data-view-log busy={pending === 'view'} onclick={() => void viewLog()}>
            {language.risuNest.diag.viewLog}
        </SettingButton>
        <SettingButton data-copy-log variant="secondary" busy={pending === 'copy'} onclick={() => void copyLog()}>
            {language.risuNest.diag.copyLog}
        </SettingButton>
    </SettingRow>
    <SettingRow label={language.risuNest.diag.consoleTitle} help={language.risuNest.diag.consoleHelp}>
        <SettingButton data-view-console-log busy={pending === 'consoleView'} onclick={() => void viewLog('console')}>
            {language.risuNest.diag.viewLog}
        </SettingButton>
        <SettingButton data-copy-console-log variant="secondary" busy={pending === 'consoleCopy'} onclick={() => void copyLog('console')}>
            {language.risuNest.diag.copyLog}
        </SettingButton>
        <SettingButton data-clear-console-log variant="secondary" onclick={clearConsole}>
            {language.risuNest.diag.clearLog}
        </SettingButton>
    </SettingRow>
    <SettingRow inline label={language.risuNest.diag.fileLog} help={language.risuNest.diag.fileLogHelp}>
        {#snippet below()}
            {#if fileLogEnabled && fileLogPath}
                <code class="mt-1 block text-xs break-all text-textcolor2" role="status" aria-live="polite">{fileLogPath}</code>
            {/if}
        {/snippet}
        <SettingToggle
            checked={fileLogEnabled}
            disabled={fileLogUpdatePending}
            label={language.risuNest.diag.fileLog}
            onchange={(enabled) => void changeFileLogging(enabled)}
        />
    </SettingRow>
</SettingGroup>
