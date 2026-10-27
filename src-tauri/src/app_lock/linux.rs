use std::{cell::RefCell, collections::HashMap, time::Duration};
use gtk::{prelude::*, gdk::prelude::GdkPixbufExt, gdk_pixbuf::{InterpType, Pixbuf}, glib};
use tauri::{AppHandle, Emitter, Manager, WebviewWindow};
use super::{desktop::{Lifecycle, color_rgb}, *};

struct Surface { cover: gtk::EventBox, style: gtk::CssProvider }
struct Runtime {
    app: AppHandle,
    state: Lifecycle,
    surfaces: HashMap<String, Surface>,
    observed: Vec<glib::WeakRef<gtk::Window>>,
    session_locked: bool,
    provisional: bool,
}
thread_local! { static RUNTIME: RefCell<Option<Runtime>> = const { RefCell::new(None) }; }

fn now() -> u64 {
    let mut time = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    if unsafe { libc::clock_gettime(libc::CLOCK_BOOTTIME, &mut time) } == 0 {
        (time.tv_sec as u64).saturating_mul(1000) + (time.tv_nsec as u64) / 1_000_000
    } else {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() as u64
    }
}

fn redraw() {
    let work = RUNTIME.with(|slot| {
        let slot = slot.borrow();
        let Some(runtime) = slot.as_ref() else { return None; };
        let (r,g,b) = color_rgb(&runtime.state.policy.background_color);
        let css = format!("* {{ background-color: rgb({r}, {g}, {b}); }}");
        Some((css, runtime.state.covered() || runtime.provisional, runtime.surfaces.values().map(|surface| (surface.cover.clone(), surface.style.clone())).collect::<Vec<_>>()))
    });
    if let Some((css, visible, surfaces)) = work {
        for (cover, style) in surfaces {
            let _ = style.load_from_data(css.as_bytes());
            cover.set_visible(visible);
            cover.queue_draw();
        }
    }
}

fn transition(active: bool) {
    let event = RUNTIME.with(|slot| {
        let mut slot = slot.borrow_mut();
        let runtime = slot.as_mut()?;
        runtime.provisional = false;
        runtime.state.transition(active, now()).map(|event| (runtime.app.clone(), event))
    });
    redraw();
    if let Some((app, event)) = event { let _ = app.emit(EVENT_NAME, event); }
}

fn reconcile() {
    let windows: Vec<gtk::Window> = gtk::Window::list_toplevels().into_iter().filter_map(|window| window.downcast().ok()).collect();
    for window in &windows {
        let observed = RUNTIME.with(|slot| {
            let mut slot = slot.borrow_mut();
            let Some(runtime) = slot.as_mut() else { return true; };
            runtime.observed.retain(|entry| entry.upgrade().is_some());
            if runtime.observed.iter().any(|entry| entry.upgrade().as_ref() == Some(window)) { return true; }
            runtime.observed.push(window.downgrade());
            false
        });
        if !observed {
            window.connect_is_active_notify(|window| {
                if !window.is_active() {
                    RUNTIME.with(|slot| {
                        if let Some(runtime) = slot.borrow_mut().as_mut() {
                            runtime.provisional = runtime.state.policy.installs_return_cover() && !runtime.state.policy.gate_required;
                        }
                    });
                    redraw();
                }
                glib::idle_add_local_once(reconcile);
            });
        }
    }
    let locked = RUNTIME.with(|slot| slot.borrow().as_ref().is_some_and(|runtime| runtime.session_locked));
    transition(!locked && windows.iter().any(|window| window.is_active()));
}

fn set_session_locked(app: &AppHandle, locked: bool) {
    let _ = app.run_on_main_thread(move || {
        RUNTIME.with(|slot| { if let Some(runtime) = slot.borrow_mut().as_mut() { runtime.session_locked = locked; } });
        reconcile();
    });
}

fn session_path<T, E>(by_pid: Result<T, E>, session_id: Option<&str>, by_id: impl FnOnce(&str) -> Result<T, E>, display: impl FnOnce() -> Result<T, E>) -> Result<T, E> {
    if let Ok(path) = by_pid { return Ok(path); }
    if let Some(id) = session_id.filter(|id| !id.is_empty()) {
        if let Ok(path) = by_id(id) { return Ok(path); }
    }
    display()
}

fn observe_session(app: AppHandle) {
    std::thread::spawn(move || {
        let result = (|| -> Result<(), Box<dyn std::error::Error>> {
            use dbus::{blocking::{Connection, stdintf::org_freedesktop_dbus::Properties}, message::MatchRule};
            let connection = Connection::new_system()?;
            let sleep_app = app.clone();
            connection.add_match(MatchRule::new_signal("org.freedesktop.login1.Manager", "PrepareForSleep"), move |(sleeping,): (bool,), _, _| {
                let app = sleep_app.clone();
                let _ = app.run_on_main_thread(move || { if sleeping { transition(false); } else { reconcile(); } });
                true
            })?;
            let observe_lock = (|| -> Result<(), dbus::Error> {
                let manager = connection.with_proxy("org.freedesktop.login1", "/org/freedesktop/login1", Duration::from_secs(5));
                let uid = unsafe { libc::getuid() };
                let validate = |path: dbus::Path<'static>| -> Result<dbus::Path<'static>, dbus::Error> {
                    let session = connection.with_proxy("org.freedesktop.login1", path.clone(), Duration::from_secs(5));
                    let (owner, _): (u32, dbus::Path<'static>) = session.get("org.freedesktop.login1.Session", "User")?;
                    if owner != uid { return Err(dbus::Error::new_failed("session belongs to another user")); }
                    Ok(path)
                };
                let by_pid: Result<(dbus::Path<'static>,), _> = manager.method_call("org.freedesktop.login1.Manager", "GetSessionByPID", (std::process::id(),));
                let path = session_path(by_pid.map(|(path,)| path), std::env::var("XDG_SESSION_ID").ok().as_deref(), |id| {
                    let (path,): (dbus::Path<'static>,) = manager.method_call("org.freedesktop.login1.Manager", "GetSession", (id,))?;
                    validate(path)
                }, || {
                    let (user,): (dbus::Path<'static>,) = manager.method_call("org.freedesktop.login1.Manager", "GetUser", (uid,))?;
                    let user = connection.with_proxy("org.freedesktop.login1", user, Duration::from_secs(5));
                    let (id, path): (String, dbus::Path<'static>) = user.get("org.freedesktop.login1.User", "Display")?;
                    if id.is_empty() { return Err(dbus::Error::new_failed("graphical session is unavailable")); }
                    validate(path)
                })?;
                let session = connection.with_proxy("org.freedesktop.login1", path.clone(), Duration::from_secs(5));
                let mut rule = MatchRule::new_signal("org.freedesktop.DBus.Properties", "PropertiesChanged");
                rule.path = Some(path);
                let changed_app = app.clone();
                connection.add_match(rule, move |(interface, values, _): (String, dbus::arg::PropMap, Vec<String>), _, _| {
                    if interface == "org.freedesktop.login1.Session" {
                        if let Some(locked) = values.get("LockedHint").and_then(|value| value.0.as_u64()) { set_session_locked(&changed_app, locked != 0); }
                    }
                    true
                })?;
                let locked: bool = session.get("org.freedesktop.login1.Session", "LockedHint")?;
                set_session_locked(&app, locked);
                Ok(())
            })();
            if let Err(error) = observe_lock { crate::nlog!("warn", "app-lock session lock observation unavailable: {error}"); }
            loop { connection.process(Duration::from_secs(10))?; }
        })();
        if let Err(error) = result { crate::nlog!("warn", "app-lock session observation unavailable: {error}"); }
    });
}

pub(super) fn setup(app: &AppHandle) -> Result<(), String> {
    let windows = app.webview_windows().into_values().map(|window| {
        let native = window.gtk_window().map_err(|error| error.to_string())?;
        let content = native.child().ok_or("app-lock window content is unavailable")?;
        Ok((window.label().to_string(), native, content))
    }).collect::<Result<Vec<_>, String>>()?;
    RUNTIME.with(|slot| *slot.borrow_mut() = Some(Runtime { app: app.clone(), state: Lifecycle::default(), surfaces: HashMap::new(), observed: Vec::new(), session_locked: false, provisional: false }));
    for (label, native, content) in windows {
        let overlay = gtk::Overlay::new();
        native.remove(&content);
        overlay.add(&content);
        let cover = gtk::EventBox::new();
        cover.set_hexpand(true);
        cover.set_vexpand(true);
        cover.set_halign(gtk::Align::Fill);
        cover.set_valign(gtk::Align::Fill);
        let style = gtk::CssProvider::new();
        cover.style_context().add_provider(&style, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
        if let Ok(source) = Pixbuf::from_read(std::io::Cursor::new(include_bytes!("../../icons/icon.png").as_slice())) {
            let icon = gtk::Image::new();
            icon.set_halign(gtk::Align::Center);
            icon.set_valign(gtk::Align::Center);
            let refresh = move |icon: &gtk::Image| {
                let scale = icon.scale_factor().max(1);
                if let Some(image) = source.scale_simple(56 * scale, 56 * scale, InterpType::Bilinear) {
                    if let Some(surface) = image.create_surface(scale, None::<&gtk::gdk::Window>) {
                        icon.set_from_surface(Some(&surface));
                    }
                }
            };
            refresh(&icon);
            icon.connect_scale_factor_notify(refresh);
            cover.add(&icon);
        }
        overlay.add_overlay(&cover);
        overlay.set_overlay_pass_through(&cover, false);
        native.add(&overlay);
        overlay.show_all();
        cover.hide();
        RUNTIME.with(|slot| { if let Some(runtime) = slot.borrow_mut().as_mut() { runtime.surfaces.insert(label, Surface { cover, style }); } });
    }
    reconcile();
    observe_session(app.clone());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::session_path;

    #[test]
    fn systemd_user_apps_resolve_their_session_without_a_pid_session() {
        assert_eq!(session_path(Ok("pid"), Some("environment"), |_| Err("unused"), || Err("unused")), Ok("pid"));
        assert_eq!(session_path(Err("no PID session"), Some("environment"), |id| Ok(id.to_owned()), || Err("unused")), Ok("environment".to_owned()));
        assert_eq!(session_path(Err("no PID session"), None, |_| Err("unused"), || Ok("display")), Ok("display"));
        assert_eq!(session_path(Err("no PID session"), Some("stale"), |_| Err("unknown or foreign session"), || Ok("display")), Ok("display"));
        assert_eq!(session_path::<&str, _>(Err("no PID session"), Some(""), |_| Ok("invalid"), || Err("no graphical session")), Err("no graphical session"));
    }
}

pub(super) async fn check(_: &AppHandle) -> Result<Capabilities, String> {
    Ok(Capabilities { auth: AuthAvailability::unsupported(), preview_supported: false })
}

pub(super) async fn authenticate(_: &WebviewWindow, _: String) -> Result<AuthOutcome, String> { Ok(AuthOutcome::Unavailable) }

async fn main_thread<T: Send + 'static>(app: &AppHandle, task: impl FnOnce(&mut Runtime) -> T + Send + 'static) -> Result<T, String> {
    let (send, receive) = tokio::sync::oneshot::channel();
    app.run_on_main_thread(move || {
        let result = RUNTIME.with(|slot| slot.borrow_mut().as_mut().map(task).ok_or_else(|| "app-lock native shell is not ready".to_string()));
        redraw();
        let _ = send.send(result);
    }).map_err(|error| error.to_string())?;
    receive.await.map_err(|error| error.to_string())?
}

pub(super) async fn set_policy(app: &AppHandle, policy: &CoverPolicy) -> Result<LifecycleSnapshot, String> {
    let policy = policy.clone();
    main_thread(app, move |runtime| { runtime.state.set_policy(policy); runtime.state.snapshot(now()) }).await
}

pub(super) async fn ready(app: &AppHandle, epoch: u64, locked: bool) -> Result<bool, String> {
    use webkit2gtk::{WebViewExt, SnapshotRegion, SnapshotOptions};
    let window = app.get_webview_window("main").ok_or_else(|| "app-lock main window is unavailable".to_string())?;
    let (send, receive) = tokio::sync::oneshot::channel();
    window.with_webview(move |native| {
        let generation = RUNTIME.with(|slot| slot.borrow_mut().as_mut().and_then(|runtime| runtime.state.begin_ready(epoch, locked)));
        let Some(generation) = generation else { let _ = send.send(Ok(false)); return; };
        let pending = RUNTIME.with(|slot| slot.borrow().as_ref().is_some_and(|runtime| runtime.state.covered()));
        if !pending {
            let ready = RUNTIME.with(|slot| slot.borrow_mut().as_mut().is_some_and(|runtime| runtime.state.finish_ready(epoch, generation, locked)));
            let _ = send.send(Ok(ready));
            return;
        }
        let view = native.inner();
        let painted_view = view.clone();
        view.snapshot(SnapshotRegion::Visible, SnapshotOptions::NONE, None::<&gtk::gio::Cancellable>, move |result| {
            if result.is_err() { let _ = send.send(Ok(false)); return; }
            let Some(clock) = painted_view.frame_clock() else { let _ = send.send(Ok(false)); return; };
            let handler = std::rc::Rc::new(RefCell::new(None));
            let remove = handler.clone();
            let send = RefCell::new(Some(send));
            *handler.borrow_mut() = Some(clock.connect_after_paint(move |clock| {
                if let Some(handler) = remove.borrow_mut().take() { clock.disconnect(handler); }
                let ready = RUNTIME.with(|slot| slot.borrow_mut().as_mut().is_some_and(|runtime| runtime.state.finish_ready(epoch, generation, locked)));
                redraw();
                if let Some(send) = send.borrow_mut().take() { let _ = send.send(Ok(ready)); }
            }));
            painted_view.queue_draw();
            clock.request_phase(gtk::gdk::FrameClockPhase::AFTER_PAINT);
        });
    }).map_err(|error| error.to_string())?;
    receive.await.map_err(|error| error.to_string())?
}
