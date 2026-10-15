use std::os::windows::io::{FromRawHandle, OwnedHandle};
use windows::core::{Error, BSTR, HRESULT, PCWSTR, PWSTR};
use windows::Win32::Foundation::{LocalFree, HANDLE, HLOCAL};
use windows::Win32::Globalization::{
    CompareStringEx, CompareStringOrdinal, CSTR_EQUAL, LOCALE_NAME_INVARIANT, NORM_IGNORECASE,
};
use windows::Win32::Security::{
    Authorization::ConvertSidToStringSidW, GetTokenInformation, LookupAccountNameW, TokenUser,
    PSID, SID_NAME_USE, TOKEN_QUERY, TOKEN_USER,
};
use windows::Win32::Storage::FileSystem::GetFullPathNameW;
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

#[derive(Debug)]
pub(super) struct Failure {
    pub stage: &'static str,
    pub code: HRESULT,
}
pub(super) type NativeResult<T> = std::result::Result<T, Failure>;
pub(super) trait At<T> {
    fn at(self, stage: &'static str) -> NativeResult<T>;
}
impl<T> At<T> for windows::core::Result<T> {
    fn at(self, stage: &'static str) -> NativeResult<T> {
        self.map_err(|error| Failure {
            stage,
            code: error.code(),
        })
    }
}
pub(super) fn diagnostic(category: &str, failure: Failure) -> String {
    format!("{category}:{}:{:08X}", failure.stage, failure.code.0 as u32)
}

pub(super) fn on_com<T: Send>(
    category: &str,
    operation: impl FnOnce() -> NativeResult<T> + Send,
) -> crate::Result<T> {
    // A fresh apartment avoids changing the GUI/runtime thread's COM mode.
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .name("sync-windows-com".into())
            .spawn_scoped(scope, || {
                unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }
                    .ok()
                    .at("com-init")?;
                struct Apartment;
                impl Drop for Apartment {
                    fn drop(&mut self) {
                        unsafe { CoUninitialize() };
                    }
                }
                let _apartment = Apartment;
                operation()
            })
            .map_err(|_| format!("{category}:worker-start"))?
            .join()
            .map_err(|_| format!("{category}:worker-failed"))?
            .map_err(|error| diagnostic(category, error))
    })
}

pub(super) fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}
pub(super) fn read_string(
    stage: &'static str,
    read: impl FnOnce(*mut BSTR) -> windows::core::Result<()>,
) -> NativeResult<String> {
    let mut value = BSTR::new();
    read(&mut value).at(stage)?;
    Ok(value.to_string())
}
pub(super) fn equal(left: &str, right: &str, ordinal: bool) -> NativeResult<bool> {
    let left: Vec<_> = left.encode_utf16().collect();
    let right: Vec<_> = right.encode_utf16().collect();
    if left.is_empty() || right.is_empty() {
        return Ok(left.is_empty() && right.is_empty());
    }
    let result = unsafe {
        if ordinal {
            CompareStringOrdinal(&left, &right, true)
        } else {
            // Match Windows PowerShell's invariant, case-insensitive -eq/-ne.
            CompareStringEx(
                LOCALE_NAME_INVARIANT,
                NORM_IGNORECASE,
                &left,
                &right,
                None,
                None,
                None,
            )
        }
    };
    if result.0 == 0 {
        return Err(Failure {
            stage: "compare",
            code: Error::from_thread().code(),
        });
    }
    Ok(result == CSTR_EQUAL)
}
pub(super) fn full_path(value: &str) -> NativeResult<String> {
    if value.is_empty() || value.contains('\0') {
        return Err(Failure {
            stage: "path",
            code: HRESULT::from_win32(123),
        });
    }
    let input = wide(value);
    let length = unsafe { GetFullPathNameW(PCWSTR(input.as_ptr()), None, None) };
    if length == 0 {
        return Err(Failure {
            stage: "path",
            code: Error::from_thread().code(),
        });
    }
    let mut buffer = vec![0; length as usize];
    let written = unsafe { GetFullPathNameW(PCWSTR(input.as_ptr()), Some(&mut buffer), None) };
    if written == 0 || written >= length {
        return Err(Failure {
            stage: "path",
            code: Error::from_thread().code(),
        });
    }
    Ok(String::from_utf16_lossy(&buffer[..written as usize]))
}
unsafe fn sid_text(sid: PSID) -> NativeResult<String> {
    let mut text = PWSTR::null();
    ConvertSidToStringSidW(sid, &mut text).at("identity")?;
    let result = text.to_string().map_err(|_| Failure {
        stage: "identity",
        code: HRESULT::from_win32(13),
    });
    let _ = LocalFree(Some(HLOCAL(text.0.cast())));
    result
}
pub(super) fn process_sid(process: HANDLE) -> NativeResult<String> {
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(process, TOKEN_QUERY, &mut token).at("identity")?;
        let _token = OwnedHandle::from_raw_handle(token.0);
        let mut length = 0;
        let _ = GetTokenInformation(token, TokenUser, None, 0, &mut length);
        if length == 0 {
            return Err(Failure {
                stage: "identity",
                code: Error::from_thread().code(),
            });
        }
        // Token information contains pointers and must have pointer alignment.
        let mut buffer = vec![0usize; (length as usize).div_ceil(size_of::<usize>())];
        GetTokenInformation(
            token,
            TokenUser,
            Some(buffer.as_mut_ptr().cast()),
            length,
            &mut length,
        )
        .at("identity")?;
        sid_text((*(buffer.as_ptr().cast::<TOKEN_USER>())).User.Sid)
    }
}
pub(super) fn current_sid() -> NativeResult<String> {
    process_sid(unsafe { GetCurrentProcess() })
}
pub(super) fn account_sid(account: &str) -> NativeResult<String> {
    if account
        .get(..4)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("S-1-"))
    {
        return Ok(account.into());
    }
    let account = wide(account);
    let mut length = 0;
    let mut domain_length = 0;
    let mut kind = SID_NAME_USE::default();
    unsafe {
        let _ = LookupAccountNameW(
            PCWSTR::null(),
            PCWSTR(account.as_ptr()),
            None,
            &mut length,
            None,
            &mut domain_length,
            &mut kind,
        );
        if length == 0 {
            return Err(Failure {
                stage: "identity",
                code: Error::from_thread().code(),
            });
        }
        let mut sid = vec![0usize; (length as usize).div_ceil(size_of::<usize>())];
        let mut domain = vec![0; domain_length as usize];
        let pointer = PSID(sid.as_mut_ptr().cast());
        LookupAccountNameW(
            PCWSTR::null(),
            PCWSTR(account.as_ptr()),
            Some(pointer),
            &mut length,
            Some(PWSTR(domain.as_mut_ptr())),
            &mut domain_length,
            &mut kind,
        )
        .at("identity")?;
        sid_text(pointer)
    }
}
