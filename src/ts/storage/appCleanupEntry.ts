import { invoke } from '@tauri-apps/api/core'
import { cleanupNeedsWebViewUpdate, type AppCleanupMode } from './appCleanup'

interface AppCleanupStatus {
    pending: boolean
    mode: AppCleanupMode | null
    error: string | null
    canCancel: boolean
}

/** A damaged record leaves this screen with nothing that can continue or cancel, so the user removes it outside the app. */
function damagedRecordInstruction(ko: boolean): string {
    const agent = navigator.userAgent
    const lead = ko ? '삭제 기록이 손상되어 초기화를 진행할 수 없습니다. ' : 'The deletion record is damaged, so the reset cannot continue. '
    if (/Android/i.test(agent)) return lead + (ko ? '앱 정보 > 저장공간에서 데이터를 삭제해주세요.' : 'Clear the data in App info > Storage.')
    if (/iPad|iPhone|iPod/.test(agent) || (navigator.platform === 'MacIntel' && navigator.maxTouchPoints > 1)) {
        return lead + (ko ? '앱을 삭제한 뒤 다시 설치해주세요.' : 'Delete the app and install it again.')
    }
    const folder = /Windows/i.test(agent) ? '%LOCALAPPDATA%\\RisuNest-cleanup'
        : /Macintosh|Mac OS X/i.test(agent) ? '~/Library/Application Support/RisuNest-cleanup'
            : '~/.local/share/risunest-cleanup'
    return lead + (ko
        ? `앱을 종료하고 ${folder} 폴더를 삭제한 뒤 RisuNest를 다시 시작하면 설정에서 초기화를 다시 진행할 수 있습니다.`
        : `Quit the app, delete the ${folder} folder, and start RisuNest again. You can then run the reset again from the settings.`)
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
    const cancel = document.createElement('button')
    cancel.textContent = ko ? '초기화 취소' : 'Cancel reset'
    cancel.style.cssText = action.style.cssText
    const cancellation = document.createElement('p')
    cancellation.textContent = ko
        ? '초기화를 취소하면 동기화와 외부 저장소를 다시 연결해야 할 수 있습니다.'
        : 'After cancelling reset, you may need to reconnect sync and external storage.'
    const updateCancellation = () => {
        cancel.hidden = cancellation.hidden = status?.canCancel !== true
        cancel.disabled = false
    }
    updateCancellation()
    const showFailure = (cause: unknown) => {
        const code = cause instanceof Error ? cause.message : cause
        message.textContent = cleanupNeedsWebViewUpdate(cause)
            ? ko ? 'Android System WebView를 업데이트한 후 로컬 데이터 삭제를 다시 시도해주세요.' : 'Update Android System WebView, then retry deleting local data.'
            : code === 'secret-index-corrupt' || code === 'cleanup-journal-corrupt'
            ? status?.canCancel === true
                ? ko ? '삭제 기록이 손상되어 초기화를 완료하지 못했습니다. 초기화 취소를 눌러 앱을 시작해주세요.' : 'The deletion record is damaged, so the reset could not finish. Select Cancel reset to start the app.'
                : damagedRecordInstruction(ko)
            : code === 'secret-cleanup-unavailable'
            ? ko ? '시스템 자격 증명 저장소를 열 수 없습니다. Linux에서는 키링을 실행하고 잠금을 해제한 후 다시 시도해주세요.' : 'The system credential store is unavailable. On Linux, start and unlock the keyring, then retry.'
            : code === 'secret-index-unavailable' || code === 'cleanup-journal-unavailable' || code === 'cleanup-files-busy-or-denied'
            ? ko ? '삭제할 파일에 접근할 수 없습니다. 파일 권한과 저장 공간을 확인한 후 다시 시도해주세요.' : 'The files could not be accessed. Check file permissions and free space, then retry.'
            : code === 'cleanup-path-redirected' || code === 'cleanup-path-overlap' || code === 'cleanup-root-overlaps-install'
            ? ko ? '현재 저장 경로에서는 로컬 데이터를 삭제할 수 없습니다.' : 'Local data cannot be deleted from the current storage location.'
            : ko ? '데이터 삭제를 완료하지 못했습니다. 다시 시도해주세요.' : 'Local data deletion could not finish. Please retry.'
        action.disabled = false
        updateCancellation()
    }
    const refreshFailure = async (cause: unknown) => {
        try { status = await invoke<AppCleanupStatus>('app_cleanup_status') }
        catch { status = undefined }
        showFailure(cause)
    }
    const retry = async () => {
        action.disabled = true
        cancel.disabled = true
        message.textContent = ko ? '데이터를 삭제하고 있습니다.' : 'Deleting local data.'
        try {
            if (!status) {
                status = await invoke<AppCleanupStatus>('app_cleanup_status')
            }
            if (!status.pending) {
                location.reload()
                return
            }
            await invoke('app_cleanup_resume')
        } catch (cause) {
            await refreshFailure(cause)
        }
    }
    action.onclick = () => { void retry() }
    cancel.onclick = () => {
        action.disabled = cancel.disabled = true
        void invoke('app_cleanup_cancel').catch(refreshFailure)
    }
    host.append(heading, message, action, cancellation, cancel)
    document.getElementById('preloading')?.remove()
    document.body.append(host)
    if (status?.pending && status.error !== null) showFailure(status.error)
    else await retry()
    await new Promise<never>(() => {})
}
