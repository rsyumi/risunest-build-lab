use std::cell::RefCell;
use tauri::ipc::Channel;

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Snapshot { completed_items: usize, total_items: usize }
thread_local! { static CHANNEL: RefCell<Option<Channel<Snapshot>>> = const { RefCell::new(None) }; }

pub(crate) fn within<T>(channel: Option<Channel<Snapshot>>, operation: impl FnOnce() -> T) -> T {
    struct Restore(Option<Channel<Snapshot>>);
    impl Drop for Restore { fn drop(&mut self) { CHANNEL.with(|slot| slot.replace(self.0.take())); } }
    let _restore = Restore(CHANNEL.with(|slot| slot.replace(channel)));
    operation()
}
pub(crate) fn plan(total: usize) -> std::sync::Arc<crate::external_storage::phase_progress::PhaseProgress> {
    let channel = CHANNEL.with(|slot| slot.borrow().clone());
    let progress = crate::external_storage::phase_progress::PhaseProgress::new(move |c| {
        if let Some(channel) = &channel { let _ = channel.send(Snapshot { completed_items: c.items as usize, total_items: c.total_items as usize }); }
    });
    progress.plan(total as u64, 0);
    progress
}
