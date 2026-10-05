import { DBState } from '../stores.svelte'
import { deletePluginDataForOwner } from './pluginDataInventory'
import { loadPlugins } from './plugins.svelte'

/**
 * Removes an installed plugin by name. With `deleteData`, the plugin's data is deleted
 * after the plugin has stopped, so nothing it saves while unloading survives the removal.
 */
export async function removeInstalledPlugin(owner: string, deleteData: boolean): Promise<void> {
    const index = DBState.db.plugins.findIndex((installed) => installed.name === owner)
    if (index !== -1) {
        if (DBState.db.currentPluginProvider === owner) DBState.db.currentPluginProvider = ''
        DBState.db.plugins.splice(index, 1)
    }
    await loadPlugins()
    if (deleteData) await deletePluginDataForOwner(owner)
}
