mod arithmetic;
pub mod cbs;
pub mod relaxed;

#[cfg(test)]
mod tests {
    use super::cbs::{self, evaluate, Budget, Context};
    #[test]
    fn cancellation_is_cooperative_and_discards_local_state() {
        let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let cancel = flag.clone();
        let handle = std::thread::spawn(move || {
            let program =
                vec![cbs::Node::Macro(vec![cbs::Node::Text(cbs::units("random::a::b"))]); 200_000];
            let mut c = Context::default();
            let mut b = Budget {
                cancel: flag,
                ..Budget::default()
            };
            evaluate(&program, &mut c, &mut b)
        });
        cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        assert_eq!(handle.join().unwrap().unwrap_err(), "cancelled");
    }
    #[test]
    fn cancellation_of_a_running_evaluator() {
        let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let cancel = flag.clone();
        let progress = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let child_progress = progress.clone();
        let handle = std::thread::spawn(move || {
            let p = vec![cbs::Node::Text(vec![65]); 400_000];
            let mut c = Context::default();
            let mut b = Budget {
                cancel: flag,
                progress: child_progress,
                ..Budget::default()
            };
            evaluate(&p, &mut c, &mut b)
        });
        while progress.load(std::sync::atomic::Ordering::Relaxed) < 100 {
            std::thread::yield_now()
        }
        cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        assert_eq!(handle.join().unwrap().unwrap_err(), "cancelled");
    }
}
