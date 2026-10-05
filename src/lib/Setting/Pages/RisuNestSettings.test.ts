import { describe, expect, it } from 'vitest'

import { readFileSync } from 'node:fs'
const settingsRawSource = readFileSync('src/lib/Setting/Settings.svelte', 'utf8')
const pageRawSource = readFileSync('src/lib/Setting/Pages/RisuNestSettings.svelte', 'utf8')
const appRawSource = readFileSync('src/App.svelte', 'utf8')
const backupRestoreRawSource = readFileSync('src/lib/Setting/Pages/RisuNestBackupRestore.svelte', 'utf8')
const storageRawSource = readFileSync('src/lib/Setting/Pages/RisuNestStorageDashboard.svelte', 'utf8')
const tauriLibRawSource = readFileSync('src-tauri/src/lib.rs', 'utf8')
const maintenanceSource = readFileSync('src/ts/storage/nativePersistentMaintenance.ts', 'utf8')
const replacementSource = readFileSync('src/ts/storage/upstreamReplacement.ts', 'utf8')
const alertSource = readFileSync('src/lib/Others/AlertComp.svelte', 'utf8')
import { languageEnglish } from 'src/lang/en'
import { languageKorean } from 'src/lang/ko'
import { RISUNEST_SETTINGS_TABS } from 'src/ts/setting/risuNestSettingsTabs'

const normalizeNewlines = (source: string) => source.replace(/\r\n?/g, '\n')
const settingsSource = normalizeNewlines(settingsRawSource)
const pageSource = normalizeNewlines(pageRawSource)
const appSource = normalizeNewlines(appRawSource)
const backupRestoreSource = normalizeNewlines(backupRestoreRawSource)
const storageSource = normalizeNewlines(storageRawSource)
const tauriLibSource = normalizeNewlines(tauriLibRawSource)

/** The source between two markers, so ordering can be asserted within one tab panel. */
function between(source: string, start: string, end: string): string {
    const from = source.indexOf(start)
    const to = source.indexOf(end, from)
    expect(from).toBeGreaterThanOrEqual(0)
    expect(to).toBeGreaterThan(from)
    return source.slice(from, to)
}

function inOrder(source: string, markers: string[]): void {
    const positions = markers.map((marker) => source.indexOf(marker))
    expect(positions.every((position) => position >= 0), markers.join(', ')).toBe(true)
    expect(positions).toEqual([...positions].sort((left, right) => left - right))
}

describe('RisuNest settings navigation', () => {
    it('places RisuNest before plugin-added entries', () => {
        expect(settingsSource).toContain('Wrench')
        expect(settingsSource).toContain('$SettingsMenuIndex === 17')
        expect(settingsSource).toContain('language.risuNest.menuTitle')
        expect(settingsSource).toContain(
            '{#each additionalSettingsMenu as menu}',
        )
        expect(
            settingsSource.indexOf('$SettingsMenuIndex === 17'),
        ).toBeLessThan(
            settingsSource.indexOf('{#each additionalSettingsMenu as menu}'),
        )
    })

    it('dispatches the RisuNest page with server settings', () => {
        expect(settingsSource).toContain('<RisuNestSettings />')
        expect(pageSource).toContain('<ServerSyncSettings />')
        expect(settingsSource).not.toContain('DeviceSyncSettings')
        expect(settingsSource).not.toContain('$SettingsMenuIndex === 18')
    })

    it('offers four tabs in order, each named in both shipped languages', () => {
        expect(RISUNEST_SETTINGS_TABS).toEqual(['settings', 'storage', 'sync', 'plugin-data'])
        expect(pageSource).toContain('role="tablist"')
        expect(pageSource).toContain('role="tab"')
        expect(pageSource).toContain('aria-selected={active}')
        expect(pageSource).toContain('role="tabpanel"')
        for (const translation of [languageEnglish, languageKorean]) {
            for (const key of ['settings', 'storage', 'sync', 'pluginData'] as const) {
                expect(translation.risuNest.tabs[key]).toEqual(expect.any(String))
            }
            expect(translation.risuNest.tabList).toEqual(expect.any(String))
        }
    })

    it('groups the settings tab as performance, UI, streaming, attachments, update, platform, diagnostics', () => {
        const panel = between(pageSource, "{#if activeTab === 'settings'}", "{:else if activeTab === 'storage'}")
        inOrder(panel, [
            '<RisuNestPerformanceSettings />',
            'items={risuNestUiSettingsItems}',
            'items={risuNestStreamingSettingsItems}',
            'items={risuNestInlaySettingsItems}',
            '<RisuNestUpdateSettings />',
            '<RisuNestAppImage />',
            '<RisuNestIOSPlatform />',
            '<RisuNestAndroidPlatform />',
            '<RisuNestLogViewer />',
        ])
        expect(panel).toContain('{#if isTauri}\n                <RisuNestUpdateSettings />')
        expect(panel).toContain('{#if isTauriAndroid}\n                <RisuNestAndroidPlatform />')
        expect(panel).toContain('{#if isTauri}\n                <RisuNestLogViewer />')
    })

    it('groups the storage tab as storage, stored attachments, data check, backup and restore', () => {
        const panel = between(pageSource, "{:else if activeTab === 'storage'}", "{:else if activeTab === 'sync'}")
        inOrder(panel, [
            '<RisuNestStorageDashboard />',
            '<RisuNestInlayInventory />',
            '<RisuNestDataHealth',
            '<RisuNestBackupRestore />',
        ])
        expect(panel).toContain('{#if isTauri}\n                <RisuNestStorageDashboard />')
        expect(panel).toContain("jumpTo('risunest-storage')")
    })

    it('groups the sync tab as sync server, external storage, local data', () => {
        const panel = between(pageSource, "{:else if activeTab === 'sync'}", '{:else}')
        inOrder(panel, [
            '<ServerSyncSettings />',
            '<ExternalStorageSettings />',
            '<RisuNestLocalData />',
        ])
        const nativeServerGroup = '{#if isTauri}\n                <ServerSyncSettings />\n            {/if}'
        expect(panel).toContain(nativeServerGroup)
        expect(panel.replace(nativeServerGroup, '')).not.toContain('<ServerSyncSettings />')
        expect(panel).toContain('{#if isTauri}\n                <RisuNestLocalData />\n            {/if}')
        const serverSettingsSource = normalizeNewlines(readFileSync('src/lib/Setting/Pages/ServerSyncSettings.svelte', 'utf8'))
        const markup = serverSettingsSource.slice(serverSettingsSource.indexOf('</script>'), serverSettingsSource.indexOf('<style>'))
        const nativeOnly = markup.slice(markup.indexOf('\n{#if isTauri}'))
        expect(nativeOnly).toContain('<SettingGroup id="risunest-server-sync" title={copy.title} description={copy.description}')
        expect(backupRestoreSource).not.toContain('ExternalStorageSettings')
    })

    it('keeps plugin data on its own tab', () => {
        const panel = between(pageSource, '{:else}\n            <RisuNestPluginData />', '{/if}')
        expect(panel).toContain('<RisuNestPluginData />')
    })

    it('opens the tab a navigation request names', () => {
        expect(pageSource).toContain('risuNestSettingsTabRequest')
        expect(appSource).toContain("openRisuNestSettingsTab('storage')")
        expect(appSource).toContain("openRisuNestSettingsTab('sync')")
        const serverNavigation = between(appSource, 'if (!isTauri || !$loadedStore) return', 'return dataHealthNavigation.subscribe(')
        inOrder(serverNavigation, [
            'serverSyncNavigation.subscribe(',
            'if (DBState.db.didFirstSetup && !$onboardingHold)',
            "openRisuNestSettingsTab('sync')",
            'SettingsMenuIndex.set(17)',
            'settingsOpen.set(true)',
        ])
        expect(appSource.indexOf("openRisuNestSettingsTab('storage')")).toBeLessThan(
            appSource.indexOf('getElementById(DATA_HEALTH_SECTION_ID)'),
        )
    })
})

describe('RisuNest backup and restore layout', () => {
    it('groups actions into file, restore, and official account rows', () => {
        for (const group of ['groupFiles', 'groupRestore', 'groupAccount']) {
            expect(backupRestoreSource).toContain(
                `{language.risuNest.backup.${group}}`,
            )
            expect(
                languageEnglish.risuNest.backup[
                    group as keyof typeof languageEnglish.risuNest.backup
                ],
            ).toEqual(expect.any(String))
            expect(
                languageKorean.risuNest.backup[
                    group as keyof typeof languageKorean.risuNest.backup
                ],
            ).toEqual(expect.any(String))
        }
        expect(backupRestoreSource).toContain('data-backup-group')
        expect(backupRestoreSource).not.toContain('className="mt-2"')
        expect(
            backupRestoreSource.indexOf(
                '{language.risuNest.backup.groupFiles}',
            ),
        ).toBeLessThan(
            backupRestoreSource.indexOf(
                '{language.risuNest.backup.groupRestore}',
            ),
        )
        expect(
            backupRestoreSource.indexOf(
                '{language.risuNest.backup.groupRestore}',
            ),
        ).toBeLessThan(
            backupRestoreSource.indexOf(
                '{language.risuNest.backup.groupAccount}',
            ),
        )
    })

    it('restores local snapshots from the storage snapshot list instead of a popup', () => {
        expect(backupRestoreSource).not.toContain('restoreLocalSnapshot')
        expect(backupRestoreSource).not.toContain('alertSelect')
        expect(storageSource).toContain('restoreNativePersistentSnapshot')
        const restoreFlow = between(maintenanceSource, 'export async function requestNativePersistentSnapshotRestore(', 'export async function restartNativeApp(')
        const confirmation = between(replacementSource, 'export async function confirmUpstreamLibraryReplacement(', '})).confirmed')
        expect(confirmation).toContain('alertCheckboxConfirm({')
        expect(confirmation).toContain('checkboxLabel: language.lwwSync.restoreAcknowledge')
        expect(confirmation).toContain('actionLabel: language.lwwSync.restoreAction')
        expect(confirmation).toContain('cancelLabel: language.lwwSync.cancelAction')
        expect(confirmation).toContain('requireChecked: true')
        inOrder(restoreFlow, [
            "runSharedNativeFileOperation('import', `snapshot-restore:${id}`",
            'runNativeSnapshotRestore(runtime, {snapshotId: id}',
            "{presentation: 'dialog', format: 'library-backup'}",
            'await copyBodies(activation)',
        ])
        expect(restoreFlow).not.toContain('pds_snapshot_restore')
        expect(tauriLibSource).not.toContain('pds_snapshot_restore_')
        expect(alertSource).toContain('disabled={dialog.requireChecked && !checkboxChecked}')
        expect(alertSource).toContain('if (confirmed && dialog.checkboxConfirm?.requireChecked && !checkboxChecked) return;')
        expect(alertSource).toContain('onclick={() => finishCheckboxConfirm(false)}>{dialog.cancelLabel}')
        const restore = storageSource.indexOf('{strings.restoreSnapshot}')
        const remove = storageSource.indexOf('{language.remove}')
        expect(restore).toBeGreaterThanOrEqual(0)
        expect(restore).toBeLessThan(remove)
    })

    it('lets the shared progress dialog report the RisuSave export', () => {
        expect(backupRestoreSource).toContain('dismissNativeFileOperationOutcome()')
        expect(backupRestoreSource).toContain('presentFileOperationError(kind, error, startedAt)')
    })
})

describe('RisuNest native command integration', () => {
    const handlerStart = tauriLibSource.indexOf('pub fn invoke_handler()')
    const handlerEnd = tauriLibSource.indexOf(
        'pub fn handle_run_event(',
        handlerStart,
    )
    const handlerSource = tauriLibSource.slice(handlerStart, handlerEnd)
    it('connects the shared command router to the product builder', () => {
        expect(handlerStart).toBeGreaterThan(0)
        expect(handlerEnd).toBeGreaterThan(handlerStart)
        expect(handlerSource).toContain('tauri::generate_handler![')
        expect(tauriLibSource.slice(0, handlerStart)).toContain(
            '.invoke_handler(invoke_handler())',
        )
    })
    const commands = [
        'pds_storage_stats',
        'pds_snapshot_delete',
        'pds_asset_gc_preview',
        'pds_asset_gc_execute',
        'native_log_tail',
        'native_log_file_path',
        'native_log_set_file_enabled',
    ]

    it.each(commands)('registers %s exactly once', (command) => {
        expect(
            handlerSource.match(new RegExp(`\\b${command}\\b`, 'g')) ?? [],
        ).toHaveLength(1)
    })

    it('registers LWW and server settings commands exactly once without a peer runtime', () => {
        expect(tauriLibSource).not.toContain('peer_sync')
        for (const command of [
            'server_sync_status',
            'server_sync_configure',
            'server_sync_lww_push',
            'server_sync_lww_pull',
            'server_sync_lww_ack',
            'server_sync_lww_fence',
            'server_sync_lww_activate',
            'server_sync_lww_hydrate',
            'server_sync_lww_inspect',
            'server_sync_lww_stage_target',
            'server_sync_lww_prepare_new_device',
            'server_sync_lww_prepare_fresh_writer',
            'server_sync_lww_activate_new_device',
            'server_sync_lww_pending_binding',
            'server_sync_lww_retry',
            'server_sync_lww_drain',
            'server_sync_lww_pending_count',
            'server_sync_progress',
            'server_sync_cancel',
            'server_sync_notify_start',
            'server_sync_notify_stop',
            'server_sync_cache_usage',
            'server_sync_cache_cleanup',
        ]) {
            expect(
                handlerSource.match(new RegExp(`\\b${command}\\b`, 'g')) ?? [],
            ).toHaveLength(1)
        }
    })
})
describe('RisuNest startup failure language schema', () => {
    const required = [
        'title',
        'schemaUnsupported',
        'storeOpen',
        'unknown',
        'restart',
        'copyDetails',
        'copied',
        'dataFolder',
        'dataPathAndroid',
        'dataPathIos',
        'stage',
    ]

    it.each([languageEnglish, languageKorean])(
        'contains every startup recovery string',
        (translation) => {
            for (const key of required) {
                expect(
                    translation.risuNest.boot[
                        key as keyof typeof translation.risuNest.boot
                    ],
                ).toEqual(expect.any(String))
            }
        },
    )
})
