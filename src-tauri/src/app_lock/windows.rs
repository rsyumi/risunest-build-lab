use std::{cell::RefCell, collections::HashMap, ptr::null_mut};
use tauri::{AppHandle, Emitter, Manager, WebviewWindow};
use ::windows::{core::{factory, HSTRING}, Security::Credentials::UI::{UserConsentVerifier, UserConsentVerifierAvailability, UserConsentVerificationResult}, Win32::System::WinRT::IUserConsentVerifierInterop};
use windows_sys::Win32::{Foundation::*, Graphics::Gdi::*, System::{RemoteDesktop::*, SystemInformation::{GetTickCount64, OSVERSIONINFOW}, Threading::GetCurrentProcessId}, UI::{Accessibility::*, HiDpi::GetDpiForWindow, Shell::{DefSubclassProc, SetWindowSubclass, RemoveWindowSubclass}, WindowsAndMessaging::*}};
use super::{desktop::{Lifecycle, color_rgb}, *};

const SUBCLASS_ID: usize = 0x524c4f43;
const RECONCILE_FOCUS: u32 = WM_APP + 0x4c;
const COVER_CLASS: &[u16] = &[82,105,115,117,78,101,115,116,76,111,99,107,67,111,118,101,114,0];

struct Surface { parent: HWND, cover: HWND }
struct Runtime {
    app: AppHandle,
    state: Lifecycle,
    surfaces: HashMap<isize, Surface>,
    session_locked: bool,
    provisional: bool,
    _foreground_hook: HWINEVENTHOOK,
}
thread_local! { static RUNTIME: RefCell<Option<Runtime>> = const { RefCell::new(None) }; }

#[link(name = "ntdll")]
extern "system" { fn RtlGetVersion(version: *mut OSVERSIONINFOW) -> i32; }

fn supported_version() -> bool {
    let mut version: OSVERSIONINFOW = unsafe { std::mem::zeroed() };
    version.dwOSVersionInfoSize = std::mem::size_of::<OSVERSIONINFOW>() as u32;
    unsafe { RtlGetVersion(&mut version) == 0 && version.dwBuildNumber >= 22000 }
}

fn now() -> u64 { unsafe { GetTickCount64() } }

fn owned(mut hwnd: HWND) -> bool {
    for _ in 0..32 {
        if hwnd.is_null() { return false; }
        let mut process = 0;
        unsafe { GetWindowThreadProcessId(hwnd, &mut process); }
        if process == unsafe { GetCurrentProcessId() } { return true; }
        hwnd = unsafe { GetWindow(hwnd, GW_OWNER) };
    }
    false
}

fn foreground_active() -> bool {
    !RUNTIME.with(|slot| slot.borrow().as_ref().is_some_and(|runtime| runtime.session_locked))
        && owned(unsafe { GetForegroundWindow() })
}

fn redraw() {
    let work = RUNTIME.with(|slot| {
        let slot = slot.borrow();
        let Some(runtime) = slot.as_ref() else { return Vec::new(); };
        let (r,g,b) = color_rgb(&runtime.state.policy.background_color);
        let color = u32::from(r) | (u32::from(g) << 8) | (u32::from(b) << 16);
        runtime.surfaces.values().map(|surface| (surface.parent, surface.cover, runtime.state.covered() || runtime.provisional, color)).collect::<Vec<_>>()
    });
    for (parent, cover, show, color) in work {
        unsafe {
            SetWindowLongPtrW(cover, GWLP_USERDATA, color as isize);
            let mut bounds = RECT::default();
            GetClientRect(parent, &mut bounds);
            SetWindowPos(cover, HWND_TOP, 0, 0, bounds.right, bounds.bottom, SWP_NOACTIVATE | if show { SWP_SHOWWINDOW } else { SWP_HIDEWINDOW });
            if show { RedrawWindow(cover, std::ptr::null(), null_mut(), RDW_INVALIDATE | RDW_UPDATENOW | RDW_ALLCHILDREN); }
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

unsafe extern "system" fn foreground_changed(_: HWINEVENTHOOK, _: u32, _: HWND, _: i32, _: i32, _: u32, _: u32) {
    transition(foreground_active());
}

unsafe extern "system" fn window_proc(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM, _: usize, _: usize) -> LRESULT {
    match message {
        WM_ACTIVATEAPP => {
            if wparam != 0 {
                let locked = RUNTIME.with(|slot| slot.borrow().as_ref().is_some_and(|runtime| runtime.session_locked));
                transition(!locked);
            } else {
                RUNTIME.with(|slot| {
                    if let Some(runtime) = slot.borrow_mut().as_mut() {
                        runtime.provisional = runtime.state.policy.installs_return_cover() && !runtime.state.policy.gate_required;
                    }
                });
                redraw();
                PostMessageW(hwnd, RECONCILE_FOCUS, 0, 0);
            }
        }
        RECONCILE_FOCUS => transition(foreground_active()),
        WM_SIZE => {
            if wparam == SIZE_MINIMIZED as usize { transition(false); }
            redraw();
        }
        WM_WTSSESSION_CHANGE => {
            if wparam == WTS_SESSION_LOCK as usize || wparam == WTS_SESSION_UNLOCK as usize {
                RUNTIME.with(|slot| { if let Some(runtime) = slot.borrow_mut().as_mut() { runtime.session_locked = wparam == WTS_SESSION_LOCK as usize; } });
                transition(foreground_active());
            }
        }
        WM_NCDESTROY => {
            WTSUnRegisterSessionNotification(hwnd);
            RemoveWindowSubclass(hwnd, Some(window_proc), SUBCLASS_ID);
            RUNTIME.with(|slot| { if let Some(runtime) = slot.borrow_mut().as_mut() { runtime.surfaces.remove(&(hwnd as isize)); } });
        }
        _ => {}
    }
    DefSubclassProc(hwnd, message, wparam, lparam)
}

unsafe extern "system" fn cover_proc(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if message == WM_PAINT {
        let mut paint = PAINTSTRUCT::default();
        let dc = BeginPaint(hwnd, &mut paint);
        let mut rect = RECT::default();
        GetClientRect(hwnd, &mut rect);
        let brush = CreateSolidBrush(GetWindowLongPtrW(hwnd, GWLP_USERDATA) as u32);
        FillRect(dc, &rect, brush);
        DeleteObject(brush);
        let parent = GetParent(hwnd);
        let mut icon = SendMessageW(parent, WM_GETICON, ICON_BIG as usize, 0) as HICON;
        if icon.is_null() { icon = GetClassLongPtrW(parent, GCLP_HICON) as HICON; }
        let size = (56 * GetDpiForWindow(parent).max(96) / 96) as i32;
        if !icon.is_null() { DrawIconEx(dc, (rect.right-size)/2, (rect.bottom-size)/2, icon, size, size, 0, null_mut(), DI_NORMAL); }
        EndPaint(hwnd, &paint);
        return 0;
    }
    if message == WM_ERASEBKGND { return 1; }
    DefWindowProcW(hwnd, message, wparam, lparam)
}

fn attach(window: &WebviewWindow) -> Result<(), String> {
    let parent = window.hwnd().map_err(|error| error.to_string())?.0 as HWND;
    if RUNTIME.with(|slot| slot.borrow().as_ref().is_some_and(|runtime| runtime.surfaces.contains_key(&(parent as isize)))) { return Ok(()); }
    unsafe {
        let instance = GetWindowLongPtrW(parent, GWLP_HINSTANCE) as HINSTANCE;
        let class = WNDCLASSW { lpfnWndProc: Some(cover_proc), hInstance: instance, lpszClassName: COVER_CLASS.as_ptr(), ..std::mem::zeroed() };
        RegisterClassW(&class);
        let cover = CreateWindowExW(0, COVER_CLASS.as_ptr(), std::ptr::null(), WS_CHILD, 0, 0, 0, 0, parent, null_mut(), instance, std::ptr::null());
        if cover.is_null() { return Err("app-lock cover creation failed".into()); }
        if SetWindowSubclass(parent, Some(window_proc), SUBCLASS_ID, 0) == 0 { DestroyWindow(cover); return Err("app-lock lifecycle hook failed".into()); }
        if WTSRegisterSessionNotification(parent, NOTIFY_FOR_THIS_SESSION) == 0 { RemoveWindowSubclass(parent, Some(window_proc), SUBCLASS_ID); DestroyWindow(cover); return Err("app-lock session hook failed".into()); }
        RUNTIME.with(|slot| { if let Some(runtime) = slot.borrow_mut().as_mut() { runtime.surfaces.insert(parent as isize, Surface { parent, cover }); } });
    }
    redraw();
    Ok(())
}

pub(super) fn setup(app: &AppHandle) -> Result<(), String> {
    let hook = unsafe { SetWinEventHook(EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND, null_mut(), Some(foreground_changed), 0, 0, WINEVENT_OUTOFCONTEXT) };
    if hook.is_null() { return Err("app-lock foreground hook failed".into()); }
    RUNTIME.with(|slot| *slot.borrow_mut() = Some(Runtime { app: app.clone(), state: Lifecycle::default(), surfaces: HashMap::new(), session_locked: false, provisional: false, _foreground_hook: hook }));
    for window in app.webview_windows().values() { attach(window)?; }
    transition(foreground_active());
    Ok(())
}

pub(super) async fn check(app: &AppHandle) -> Result<Capabilities, String> {
    let auth = if !supported_version() { AuthAvailability::unsupported() } else {
        let (send, receive) = tokio::sync::oneshot::channel();
        app.run_on_main_thread(move || { let _ = send.send(UserConsentVerifier::CheckAvailabilityAsync()); }).map_err(|error| error.to_string())?;
        match receive.await.map_err(|error| error.to_string())? {
            Ok(operation) => match operation.await {
                Ok(UserConsentVerifierAvailability::Available) => AuthAvailability { status: AuthStatus::Available, method: Some(AuthMethod::WindowsHello) },
                _ => AuthAvailability::unsupported(),
            },
            Err(_) => AuthAvailability::unsupported(),
        }
    };
    Ok(Capabilities { auth, preview_supported: false })
}

pub(super) async fn authenticate(window: &WebviewWindow, reason: String) -> Result<AuthOutcome, String> {
    if !supported_version() { return Ok(AuthOutcome::Unavailable); }
    let handle = window.hwnd().map_err(|error| error.to_string())?.0 as isize;
    let (send, receive) = tokio::sync::oneshot::channel();
    window.run_on_main_thread(move || {
        let result = (|| {
            let interop: IUserConsentVerifierInterop = factory::<UserConsentVerifier, IUserConsentVerifierInterop>()?;
            let operation: windows_future::IAsyncOperation<UserConsentVerificationResult> = unsafe { interop.RequestVerificationForWindowAsync(::windows::Win32::Foundation::HWND(handle as *mut _), &HSTRING::from(reason))? };
            ::windows::core::Result::Ok(operation)
        })();
        let _ = send.send(result);
    }).map_err(|error| error.to_string())?;
    let outcome = match receive.await.map_err(|error| error.to_string())? {
        Ok(operation) => match operation.await {
            Ok(UserConsentVerificationResult::Verified) => AuthOutcome::Success,
            Ok(UserConsentVerificationResult::Canceled) => AuthOutcome::Cancelled,
            Ok(UserConsentVerificationResult::DeviceNotPresent | UserConsentVerificationResult::NotConfiguredForUser | UserConsentVerificationResult::DisabledByPolicy) => AuthOutcome::Unavailable,
            _ => AuthOutcome::Failed,
        },
        Err(_) => AuthOutcome::Unavailable,
    };
    Ok(outcome)
}

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
    let window = app.get_webview_window("main").ok_or_else(|| "app-lock main window is unavailable".to_string())?;
    let (send, receive) = tokio::sync::oneshot::channel();
    window.with_webview(move |native| {
        let send = std::rc::Rc::new(RefCell::new(Some(send)));
        let complete = send.clone();
        let generation = RUNTIME.with(|slot| slot.borrow_mut().as_mut().and_then(|runtime| runtime.state.begin_ready(epoch, locked)));
        let Some(generation) = generation else {
            if let Some(send) = send.borrow_mut().take() { let _ = send.send(Ok(false)); }
            return;
        };
        let pending = RUNTIME.with(|slot| slot.borrow().as_ref().is_some_and(|runtime| runtime.state.covered()));
        if !pending {
            let ready = RUNTIME.with(|slot| slot.borrow_mut().as_mut().is_some_and(|runtime| runtime.state.finish_ready(epoch, generation, locked)));
            if let Some(send) = send.borrow_mut().take() { let _ = send.send(Ok(ready)); }
            return;
        }
        let capture = (|| -> ::windows::core::Result<()> {
            use ::windows::Win32::{Foundation::HGLOBAL, Graphics::Dwm::DwmFlush, System::Com::StructuredStorage::CreateStreamOnHGlobal};
            use webview2_com::{CapturePreviewCompletedHandler, Microsoft::Web::WebView2::Win32::COREWEBVIEW2_CAPTURE_PREVIEW_IMAGE_FORMAT_PNG};
            unsafe {
                let view = native.controller().CoreWebView2()?;
                let stream = CreateStreamOnHGlobal(HGLOBAL::default(), true)?;
                let keep_stream = stream.clone();
                view.CapturePreview(COREWEBVIEW2_CAPTURE_PREVIEW_IMAGE_FORMAT_PNG, &stream, &CapturePreviewCompletedHandler::create(Box::new(move |result| {
                    let _stream = keep_stream;
                    let result = result.and_then(|_| DwmFlush()).map(|_| {
                        transition(foreground_active());
                        let ready = RUNTIME.with(|slot| slot.borrow_mut().as_mut().is_some_and(|runtime| runtime.state.finish_ready(epoch, generation, locked)));
                        redraw();
                        ready
                    }).unwrap_or(false);
                    if let Some(send) = complete.borrow_mut().take() { let _ = send.send(Ok(result)); }
                    Ok(())
                })))?;
            }
            Ok(())
        })();
        if capture.is_err() {
            if let Some(send) = send.borrow_mut().take() { let _ = send.send(Ok(false)); }
        }
    }).map_err(|error| error.to_string())?;
    receive.await.map_err(|error| error.to_string())?
}
