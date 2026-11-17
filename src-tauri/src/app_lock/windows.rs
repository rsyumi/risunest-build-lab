use std::{cell::RefCell, collections::HashMap, ptr::null_mut, sync::OnceLock};
use tauri::{AppHandle, Emitter, Manager, WebviewWindow};
use ::windows::{core::{factory, HSTRING}, Security::Credentials::UI::{UserConsentVerifier, UserConsentVerifierAvailability, UserConsentVerificationResult}, Win32::System::WinRT::IUserConsentVerifierInterop};
use windows_sys::Win32::{Foundation::*, Graphics::Gdi::*, System::{RemoteDesktop::*, SystemInformation::{GetTickCount64, OSVERSIONINFOW}, Threading::GetCurrentProcessId}, UI::{Accessibility::*, HiDpi::GetDpiForWindow, Shell::{DefSubclassProc, SetWindowSubclass, RemoveWindowSubclass}, WindowsAndMessaging::*}};
use super::{desktop::{Lifecycle, color_rgb}, *};

const SUBCLASS_ID: usize = 0x524c4f43;
const RECONCILE_FOCUS: u32 = WM_APP + 0x4c;
const COVER_CLASS: &[u16] = &[82,105,115,117,78,101,115,116,76,111,99,107,67,111,118,101,114,0];

struct CoverIcon { handle: HICON, size: u32 }
impl Drop for CoverIcon {
    fn drop(&mut self) { unsafe { DestroyIcon(self.handle); } }
}
struct Surface { parent: HWND, cover: HWND, icon: CoverIcon }
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

fn icon_source() -> Result<&'static image::RgbaImage, String> {
    static IMAGE: OnceLock<Result<image::RgbaImage, String>> = OnceLock::new();
    IMAGE.get_or_init(|| image::load_from_memory(include_bytes!("../../icons/icon.png"))
        .map(|image| image.into_rgba8()).map_err(|error| error.to_string())).as_ref().map_err(Clone::clone)
}

fn create_icon(size: u32) -> Result<CoverIcon, String> {
    create_icon_from_image(icon_source()?, size)
}

fn create_icon_from_image(source: &image::RgbaImage, size: u32) -> Result<CoverIcon, String> {
    let mut premultiplied = source.clone();
    for pixel in premultiplied.pixels_mut() {
        let alpha = u16::from(pixel.0[3]);
        for channel in &mut pixel.0[..3] { *channel = ((u16::from(*channel) * alpha + 127) / 255) as u8; }
    }
    let image = image::imageops::resize(&premultiplied, size, size, image::imageops::FilterType::Lanczos3);
    let mut info: BITMAPINFO = unsafe { std::mem::zeroed() };
    info.bmiHeader = BITMAPINFOHEADER {
        biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
        biWidth: size as i32, biHeight: -(size as i32), biPlanes: 1, biBitCount: 32,
        biCompression: BI_RGB, ..unsafe { std::mem::zeroed() }
    };
    unsafe {
        let mut pixels = null_mut();
        let color = CreateDIBSection(null_mut(), &info, DIB_RGB_COLORS, &mut pixels, null_mut(), 0);
        if color.is_null() { return Err("app-lock icon bitmap creation failed".into()); }
        let output = std::slice::from_raw_parts_mut(pixels.cast::<u8>(), (size * size * 4) as usize);
        let mask_stride = size.div_ceil(16) as usize * 2;
        let mut mask_bits = vec![0u8; mask_stride * size as usize];
        for (index, (source, target)) in image.pixels().zip(output.chunks_exact_mut(4)).enumerate() {
            let [r, g, b, a] = source.0;
            target.copy_from_slice(&[b, g, r, a]);
            if a == 0 { mask_bits[index / size as usize * mask_stride + index % size as usize / 8] |= 0x80 >> (index % size as usize % 8); }
        }
        let mask = CreateBitmap(size as i32, size as i32, 1, 1, mask_bits.as_ptr().cast());
        if mask.is_null() { DeleteObject(color); return Err("app-lock icon mask creation failed".into()); }
        let handle = CreateIconIndirect(&ICONINFO { fIcon: 1, xHotspot: 0, yHotspot: 0, hbmMask: mask, hbmColor: color });
        DeleteObject(mask);
        DeleteObject(color);
        if handle.is_null() { return Err("app-lock icon creation failed".into()); }
        Ok(CoverIcon { handle, size })
    }
}

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
        let size = (56 * GetDpiForWindow(parent).max(96) / 96) as i32;
        let icon = RUNTIME.with(|slot| {
            let mut slot = slot.borrow_mut();
            let surface = slot.as_mut()?.surfaces.get_mut(&(parent as isize))?;
            if surface.icon.size != size as u32 {
                if let Ok(icon) = create_icon(size as u32) { surface.icon = icon; }
            }
            Some(surface.icon.handle)
        }).unwrap_or(null_mut());
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
    let icon = create_icon(56 * unsafe { GetDpiForWindow(parent) }.max(96) / 96)?;
    unsafe {
        let instance = GetWindowLongPtrW(parent, GWLP_HINSTANCE) as HINSTANCE;
        let class = WNDCLASSW { lpfnWndProc: Some(cover_proc), hInstance: instance, lpszClassName: COVER_CLASS.as_ptr(), ..std::mem::zeroed() };
        RegisterClassW(&class);
        let cover = CreateWindowExW(WS_EX_LAYERED, COVER_CLASS.as_ptr(), std::ptr::null(), WS_CHILD, 0, 0, 0, 0, parent, null_mut(), instance, std::ptr::null());
        if cover.is_null() { return Err(format!("app-lock cover creation failed: {}", std::io::Error::last_os_error())); }
        if SetLayeredWindowAttributes(cover, 0, 255, LWA_ALPHA) == 0 { DestroyWindow(cover); return Err("app-lock cover composition failed".into()); }
        if SetWindowSubclass(parent, Some(window_proc), SUBCLASS_ID, 0) == 0 { DestroyWindow(cover); return Err("app-lock lifecycle hook failed".into()); }
        if WTSRegisterSessionNotification(parent, NOTIFY_FOR_THIS_SESSION) == 0 { RemoveWindowSubclass(parent, Some(window_proc), SUBCLASS_ID); DestroyWindow(cover); return Err("app-lock session hook failed".into()); }
        RUNTIME.with(|slot| { if let Some(runtime) = slot.borrow_mut().as_mut() { runtime.surfaces.insert(parent as isize, Surface { parent, cover, icon }); } });
    }
    redraw();
    Ok(())
}

pub(super) fn setup(app: &AppHandle) -> Result<(), String> {
    let hook = unsafe { SetWinEventHook(EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND, null_mut(), Some(foreground_changed), 0, 0, WINEVENT_OUTOFCONTEXT) };
    if hook.is_null() { return Err("app-lock foreground hook failed".into()); }
    RUNTIME.with(|slot| *slot.borrow_mut() = Some(Runtime { app: app.clone(), state: Lifecycle::default(), surfaces: HashMap::new(), session_locked: false, provisional: false, _foreground_hook: hook }));
    for window in app.webview_windows().values() {
        if let Err(error) = attach(window) {
            let runtime = RUNTIME.with(|slot| slot.borrow_mut().take());
            if let Some(runtime) = runtime {
                unsafe {
                    UnhookWinEvent(runtime._foreground_hook);
                    for surface in runtime.surfaces.values() {
                        WTSUnRegisterSessionNotification(surface.parent);
                        RemoveWindowSubclass(surface.parent, Some(window_proc), SUBCLASS_ID);
                        DestroyWindow(surface.cover);
                    }
                }
            }
            return Err(error);
        }
    }
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
        transition(foreground_active());
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
        transition(foreground_active());
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cover_icon_keeps_high_dpi_resolution_and_transparent_edge_color() {
        let source = icon_source().unwrap();
        assert!(source.width() >= 112 && source.height() >= 112);
        let icon = create_icon(112).unwrap();
        unsafe {
            let mut info: ICONINFO = std::mem::zeroed();
            assert_ne!(GetIconInfo(icon.handle, &mut info), 0);
            let mut bitmap: BITMAP = std::mem::zeroed();
            let read = GetObjectW(info.hbmColor, std::mem::size_of::<BITMAP>() as i32, (&mut bitmap as *mut BITMAP).cast());
            DeleteObject(info.hbmColor);
            DeleteObject(info.hbmMask);
            assert_ne!(read, 0);
            assert_eq!((bitmap.bmWidth, bitmap.bmHeight, bitmap.bmBitsPixel), (112, 112, 32));

            let edge = image::RgbaImage::from_fn(2, 2, |x, _| if x == 0 { image::Rgba([255, 0, 0, 255]) } else { image::Rgba([0, 0, 0, 0]) });
            let icon = create_icon_from_image(&edge, 1).unwrap();
            assert_ne!(GetIconInfo(icon.handle, &mut info), 0);
            let dc = CreateCompatibleDC(null_mut());
            assert!(!dc.is_null());
            let mut dib: BITMAPINFO = std::mem::zeroed();
            dib.bmiHeader = BITMAPINFOHEADER { biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: 1, biHeight: -1, biPlanes: 1, biBitCount: 32, biCompression: BI_RGB, ..std::mem::zeroed() };
            let mut pixel = [0u8; 4];
            let read = GetDIBits(dc, info.hbmColor, 0, 1, pixel.as_mut_ptr().cast(), &mut dib, DIB_RGB_COLORS);
            DeleteDC(dc);
            DeleteObject(info.hbmColor);
            DeleteObject(info.hbmMask);
            assert_eq!(read, 1);
            assert_eq!((pixel[0], pixel[1]), (0, 0));
            assert!((100..=150).contains(&pixel[3]));
            assert_eq!(pixel[2], pixel[3], "transparent red edges must not darken during scaling");
        }
    }
}
