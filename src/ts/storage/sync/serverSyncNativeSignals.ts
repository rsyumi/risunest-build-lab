/** Native change notifications for the synchronisation scheduler. They carry no
 * payload: a notification only says that something may have moved, and the run
 * it brings forward confirms the remote head. */
export const SERVER_SYNC_DEVICE_CHANGED_EVENT = "risu-server-sync-device-changed";
export const SERVER_SYNC_REMOTE_HINT_EVENT = "risu-server-sync-remote-hint";
