use std::sync::Mutex;

pub(super) enum Event {
    Activated,
    BannerHidden,
    Dismissed,
}

pub(super) struct ActivationCallback {
    callback: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl ActivationCallback {
    pub(super) fn new(callback: impl FnOnce() + Send + 'static) -> Self {
        Self {
            callback: Mutex::new(Some(Box::new(callback))),
        }
    }

    pub(super) fn dispatch(&self, event: Event) {
        if matches!(event, Event::BannerHidden) {
            return;
        }
        let callback = self.callback.lock().ok().and_then(|mut value| value.take());
        if matches!(event, Event::Activated) {
            if let Some(callback) = callback {
                callback();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    #[test]
    fn banner_timeout_keeps_the_callback_for_a_later_activation() {
        let actions = Arc::new(Mutex::new(Vec::new()));
        let captured = actions.clone();
        let lifetime = Arc::new(());
        let observer = Arc::downgrade(&lifetime);
        let owner = ActivationCallback::new(move || {
            let _lifetime = lifetime;
            captured
                .lock()
                .unwrap()
                .extend(["show", "unminimize", "focus"]);
        });
        assert!(actions.lock().unwrap().is_empty());
        for _ in 0..2 {
            owner.dispatch(Event::BannerHidden);
        }
        assert!(actions.lock().unwrap().is_empty());
        assert!(observer.upgrade().is_some());
        owner.dispatch(Event::Activated);
        owner.dispatch(Event::Activated);
        assert_eq!(*actions.lock().unwrap(), ["show", "unminimize", "focus"]);
        assert!(observer.upgrade().is_none());
    }

    #[test]
    fn actual_dismissal_and_teardown_release_without_activation() {
        let actions = Arc::new(AtomicUsize::new(0));
        for dismiss in [true, false] {
            let captured = actions.clone();
            let lifetime = Arc::new(());
            let observer = Arc::downgrade(&lifetime);
            let owner = ActivationCallback::new(move || {
                let _lifetime = lifetime;
                captured.fetch_add(1, Ordering::SeqCst);
            });
            owner.dispatch(Event::BannerHidden);
            if dismiss {
                owner.dispatch(Event::Dismissed);
                owner.dispatch(Event::Activated);
            }
            drop(owner);
            assert!(observer.upgrade().is_none());
        }
        assert_eq!(actions.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn concurrent_callbacks_activate_only_once() {
        let count = Arc::new(AtomicUsize::new(0));
        let captured = count.clone();
        let owner = ActivationCallback::new(move || {
            captured.fetch_add(1, Ordering::SeqCst);
        });
        std::thread::scope(|scope| {
            for _ in 0..4 {
                scope.spawn(|| owner.dispatch(Event::Activated));
            }
        });
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }
}
