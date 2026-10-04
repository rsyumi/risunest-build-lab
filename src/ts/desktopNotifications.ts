import { invoke } from '@tauri-apps/api/core'

/** Shows a system notification through the native notification plugin. */
export async function notifyDesktop(body: string): Promise<void> {
    await invoke('plugin:notification|notify', { options: { title: 'RisuNest', body } })
}

export async function requestDesktopNotifications(): Promise<boolean> {
    return await invoke('plugin:notification|request_permission') === 'granted'
}
