import { invoke } from '@tauri-apps/api/core'

export async function relaunch(): Promise<void> {
    await invoke('desktop_relaunch')
}
