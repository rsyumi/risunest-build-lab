use super::*;
use std::cell::Cell;

#[test]
fn output_hook_restores_prior_callback_on_return_and_unwind_without_borrowing_it() {
    let calls = Rc::new(Cell::new(0));
    let original = calls.clone();
    let _outer = HeldHook::install(Box::new(move || original.set(original.get() + 1)));
    let failure = std::panic::catch_unwind(|| {
        let _inner = HeldHook::install(Box::new(|| panic!("inner must be restored away")));
        panic!("synthetic callback unwind");
    });
    assert!(failure.is_err());
    lww::BEFORE_HELD_COMMIT.with(|slot| slot.borrow_mut().take()).unwrap()();
    assert_eq!(calls.get(), 1);
    let reentrant = HeldHook::install(Box::new(|| {
        let _nested = HeldHook::install(Box::new(|| {}));
    }));
    lww::BEFORE_HELD_COMMIT.with(|slot| slot.borrow_mut().take()).unwrap()();
    drop(reentrant);
}

#[tokio::test]
async fn task_local_observations_do_not_leak_to_unscoped_tasks() {
    use super::super::controller::Controller;
    let controller = Controller::default();
    let ticket = serde_json::from_value(serde_json::json!({"epoch":"synthetic", "sequence":"1"})).unwrap();
    let token = controller.arm(ticket, Gate::None).unwrap();
    let session = controller.session(&token).unwrap();
    CURRENT.scope(session.clone(), async {
        observe(Kind::RequestFormatted);
        assert!(tokio::spawn(async { current().is_none() }).await.unwrap());
    }).await;
    assert!(current().is_none());
    assert_eq!(session.snapshot().events.len(), 1);
}
