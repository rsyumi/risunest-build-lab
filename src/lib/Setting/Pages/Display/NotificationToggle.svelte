<script lang="ts">
  import { language } from "src/lang";
  import { isTauriIOS, isTauriAndroid, isTauriDesktop } from "src/ts/platform";
  import { requestDesktopNotifications } from "src/ts/desktopNotifications";
  import { requestAndroidGenerationNotifications } from "src/ts/androidGenerationKeepAlive";
  import { requestIOSNotifications } from "src/ts/iosNative";
  import { alertError } from "src/ts/alert";
  import { DBState } from "src/ts/stores.svelte";
  import Check from "src/lib/UI/GUI/CheckInput.svelte";
</script>

<div class="flex items-center mt-2">
  <Check
    bind:check={DBState.db.notification}
    name={language.notification}
    onChange={async () => {
      if (isTauriIOS) {
        if (!DBState.db.notification) return;
        try {
          if (!(await requestIOSNotifications()).granted) {
            DBState.db.notification = false;
            alertError(language.permissionDenied);
          }
        } catch (error) {
          DBState.db.notification = false;
          alertError(String(error));
        }
        return;
      }
      if (!DBState.db.notification) return;
      try {
        let granted = false;
        if (isTauriAndroid) {
          await requestAndroidGenerationNotifications();
          granted = await window.RisuCompletionNotifications?.enabled() === true;
        } else if (isTauriDesktop) {
          granted = await requestDesktopNotifications();
        } else if (typeof Notification !== 'undefined') {
          granted = await Notification.requestPermission() === 'granted';
        }
        if (!granted) {
          DBState.db.notification = false;
          alertError(language.permissionDenied);
        }
      } catch {
        DBState.db.notification = false;
        alertError(language.permissionDenied);
      }
    }}
  />
</div>
