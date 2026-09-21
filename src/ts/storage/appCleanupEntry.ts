import { invoke } from '@tauri-apps/api/core'
import { cleanupNeedsWebViewUpdate, type AppCleanupMode } from './appCleanup'

interface AppCleanupStatus {
    pending: boolean
    mode: AppCleanupMode | null
    error: string | null
}

/** This entry must remain independent of application storage and plugins. */
export async function appCleanupBeforeBootstrap(): Promise<void> {
    if (!(window as Window & { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__) return
    let status: AppCleanupStatus | undefined
    try {
        status = await invoke<AppCleanupStatus>('app_cleanup_status')
        if (status.pending === false) return
    } catch {
        // A failed status check cannot establish that normal startup is safe.
    }
    const ko = navigator.language.startsWith('ko')
    const host = document.createElement('main')
    host.style.cssText = 'font:16px system-ui;padding:2rem;max-width:42rem;margin:auto;line-height:1.6;color:var(--risu-theme-textcolor,inherit);background:var(--risu-theme-bgcolor,transparent)'
    const heading = document.createElement('h1')
    heading.textContent = ko ? 'RisuNest 초기화' : 'RisuNest reset'
    const message = document.createElement('p')
    message.setAttribute('role', 'status')
    const action = document.createElement('button')
    action.textContent = ko ? '다시 시도' : 'Retry'
    action.style.cssText = 'font:inherit;padding:.5rem 1rem;cursor:pointer'
    const showFailure = (cause: unknown) => {
        message.textContent = cleanupNeedsWebViewUpdate(cause)
            ? ko ? 'Android System WebView를 업데이트한 후 로컬 데이터 삭제를 다시 시도해주세요.' : 'Update Android System WebView, then retry deleting local data.'
            : ko ? '데이터 삭제를 완료하지 못했습니다. 다시 시도해주세요.' : 'Local data deletion could not finish. Please retry.'
        action.disabled = false
    }
    const retry = async () => {
        action.disabled = true
        message.textContent = ko ? '데이터를 삭제하고 있습니다.' : 'Deleting local data.'
        try {
            if (!status) {
                status = await invoke<AppCleanupStatus>('app_cleanup_status')
                if (!status.pending) {
                    location.reload()
                    return
                }
            }
            await invoke('app_cleanup_resume')
        } catch (cause) {
            showFailure(cause)
        }
    }
    action.onclick = () => { void retry() }
    host.append(heading, message, action)
    document.getElementById('preloading')?.remove()
    document.body.append(host)
    if (status?.pending && status.error !== null) showFailure(status.error)
    else await retry()
    await new Promise<never>(() => {})
}
