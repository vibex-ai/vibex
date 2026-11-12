//! Windows registry access for the per-user `Path`.
//!
//! Everything here exists because NSIS cannot carry a value longer than
//! `NSIS_MAX_STRLEN - 1` (1023 by default). `RegQueryValueExW` is called once
//! with a null buffer to ask the registry for the exact byte length, then again
//! with a buffer of that size. A value that grows between the two calls is read
//! again, not truncated.
//!
//! The value type is preserved when writing. A `Path` stored as
//! `REG_EXPAND_SZ` stays expandable; writing it as `REG_SZ` would freeze
//! `%USERPROFILE%`-style references at install time. Unknown types are refused
//! instead of being rewritten as strings.

use std::fmt;
use std::io;
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_MORE_DATA, ERROR_SUCCESS};
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, REG_EXPAND_SZ, REG_NONE, REG_OPTION_NON_VOLATILE,
    REG_SZ, REG_VALUE_TYPE, RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegQueryValueExW,
    RegSetValueExW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    HWND_BROADCAST, SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_SETTINGCHANGE,
};

use crate::path::{self, Removal};
use crate::{PATH_VALUE_NAME, USER_ENVIRONMENT_KEY};

/// How many times a value that keeps growing between the size query and the
/// read is retried before giving up. Concurrent edits are rare, and refusing is
/// safer than writing a value assembled from a truncated read.
const READ_ATTEMPTS: usize = 4;

#[derive(Debug)]
pub enum Error {
    Open { path: String, code: u32 },
    Read { code: u32 },
    Write { code: u32 },
    Delete { code: u32 },
    UnsupportedType { kind: u32 },
    InvalidUtf16,
    ValueChangedWhileReading,
    TooLarge,
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Open { path, code } => write!(
                formatter,
                "cannot open HKCU\\{path}: {}",
                io::Error::from_raw_os_error(*code as i32)
            ),
            Self::Read { code } => write!(
                formatter,
                "cannot read the user Path: {}",
                io::Error::from_raw_os_error(*code as i32)
            ),
            Self::Write { code } => write!(
                formatter,
                "cannot write the user Path: {}",
                io::Error::from_raw_os_error(*code as i32)
            ),
            Self::Delete { code } => write!(
                formatter,
                "cannot delete the user Path: {}",
                io::Error::from_raw_os_error(*code as i32)
            ),
            Self::UnsupportedType { kind } => write!(
                formatter,
                "the Path uses registry value type {kind}, which this tool does not rewrite"
            ),
            Self::InvalidUtf16 => write!(formatter, "the user Path is not valid UTF-16"),
            Self::ValueChangedWhileReading => {
                write!(formatter, "the user Path kept changing while it was read")
            }
            Self::TooLarge => write!(formatter, "the user Path is too large to write"),
        }
    }
}

impl std::error::Error for Error {}

/// Add `directory` to the per-user `Path`.
///
/// Returns whether the registry changed. A failed read never turns into an
/// overwrite: the error is returned to the caller before any write happens.
pub fn add_directory(directory: &str) -> Result<bool, Error> {
    add_to(USER_ENVIRONMENT_KEY, PATH_VALUE_NAME, directory)
}

/// Remove `directory` from the per-user `Path`.
///
/// Returns whether the registry changed. Deleting the value is only chosen when
/// no other directory (or empty entry) remains.
pub fn remove_directory(directory: &str) -> Result<bool, Error> {
    remove_from(USER_ENVIRONMENT_KEY, PATH_VALUE_NAME, directory)
}

fn add_to(key_path: &str, value_name: &str, directory: &str) -> Result<bool, Error> {
    modify(key_path, value_name, |current| {
        path::append(current, directory).map_or(Change::Unchanged, |value| Change::Set(Some(value)))
    })
}

fn remove_from(key_path: &str, value_name: &str, directory: &str) -> Result<bool, Error> {
    modify(key_path, value_name, |current| {
        let Some(current) = current else {
            return Change::Unchanged;
        };
        match path::remove(current, directory) {
            Removal::Unchanged => Change::Unchanged,
            Removal::DeleteValue => Change::Set(None),
            Removal::Remaining(value) => Change::Set(Some(value)),
        }
    })
}

enum Change {
    Unchanged,
    /// `None` deletes the value entirely.
    Set(Option<String>),
}

struct Value {
    text: String,
    kind: REG_VALUE_TYPE,
}

fn modify<F>(key_path: &str, value_name: &str, change: F) -> Result<bool, Error>
where
    F: FnOnce(Option<&str>) -> Change,
{
    let key = open_key(key_path)?;
    let name = wide(value_name);
    let existing = read_value(key.0, &name)?;

    match change(existing.as_ref().map(|value| value.text.as_str())) {
        Change::Unchanged => Ok(false),
        Change::Set(None) => {
            delete_value(key.0, &name)?;
            broadcast_environment_change();
            Ok(true)
        }
        Change::Set(Some(text)) => {
            // An existing empty `REG_NONE` slot becomes a normal expandable
            // string; every real string type keeps its original type.
            let kind = match existing {
                Some(Value { kind: REG_NONE, .. }) | None => REG_EXPAND_SZ,
                Some(Value { kind, .. }) => kind,
            };
            write_value(key.0, &name, &text, kind)?;
            broadcast_environment_change();
            Ok(true)
        }
    }
}

struct Key(HKEY);

impl Drop for Key {
    fn drop(&mut self) {
        // The handle came from a successful open and is valid until here.
        unsafe {
            RegCloseKey(self.0);
        }
    }
}

fn open_key(key_path: &str) -> Result<Key, Error> {
    let name = wide(key_path);
    let mut key: HKEY = null_mut();
    let status = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            name.as_ptr(),
            0,
            null(),
            REG_OPTION_NON_VOLATILE,
            KEY_READ | KEY_WRITE,
            null(),
            &mut key,
            null_mut(),
        )
    };
    if status == ERROR_SUCCESS {
        Ok(Key(key))
    } else {
        Err(Error::Open {
            path: key_path.to_owned(),
            code: status,
        })
    }
}

fn read_value(key: HKEY, name: &[u16]) -> Result<Option<Value>, Error> {
    let mut kind: REG_VALUE_TYPE = REG_NONE;
    let mut size = 0u32;
    let status =
        unsafe { RegQueryValueExW(key, name.as_ptr(), null(), &mut kind, null_mut(), &mut size) };
    match status {
        ERROR_SUCCESS => {}
        ERROR_FILE_NOT_FOUND => return Ok(None),
        other => return Err(Error::Read { code: other }),
    }

    if size == 0 {
        return decode(Vec::new(), kind).map(Some);
    }

    for _ in 0..READ_ATTEMPTS {
        // `Vec<u8>` has the same alignment requirement as the `u16` data it
        // receives, and the registry writes the value as little-endian UTF-16.
        let mut buffer = vec![0u8; size as usize];
        let mut written = size;
        let status = unsafe {
            RegQueryValueExW(
                key,
                name.as_ptr(),
                null(),
                &mut kind,
                buffer.as_mut_ptr(),
                &mut written,
            )
        };
        match status {
            ERROR_SUCCESS => {
                if written as usize > buffer.len() || !written.is_multiple_of(2) {
                    return Err(Error::InvalidUtf16);
                }
                buffer.truncate(written as usize);
                return decode(buffer, kind).map(Some);
            }
            ERROR_MORE_DATA => {
                // On this error `written` is the new required size. If it did
                // not grow, retrying would loop forever.
                if written <= size {
                    return Err(Error::ValueChangedWhileReading);
                }
                size = written;
            }
            ERROR_FILE_NOT_FOUND => return Ok(None),
            other => return Err(Error::Read { code: other }),
        }
    }
    Err(Error::ValueChangedWhileReading)
}

fn decode(bytes: Vec<u8>, kind: REG_VALUE_TYPE) -> Result<Value, Error> {
    if !matches!(kind, REG_SZ | REG_EXPAND_SZ | REG_NONE) {
        return Err(Error::UnsupportedType { kind });
    }
    if !bytes.len().is_multiple_of(2) {
        return Err(Error::InvalidUtf16);
    }

    let mut units = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes(*pair))
        .collect::<Vec<_>>();
    if units.last() == Some(&0) {
        units.pop();
    }
    let text = String::from_utf16(&units).map_err(|_| Error::InvalidUtf16)?;
    // An embedded NUL would make Windows see a shorter value than the registry
    // holds. That is data this tool does not understand, so leave it alone.
    if text.contains('\0') {
        return Err(Error::InvalidUtf16);
    }
    if kind == REG_NONE && !text.is_empty() {
        return Err(Error::UnsupportedType { kind });
    }

    Ok(Value { text, kind })
}

fn write_value(key: HKEY, name: &[u16], text: &str, kind: REG_VALUE_TYPE) -> Result<(), Error> {
    let mut data = text.encode_utf16().collect::<Vec<_>>();
    data.push(0);
    let bytes = data
        .len()
        .checked_mul(std::mem::size_of::<u16>())
        .and_then(|bytes| u32::try_from(bytes).ok())
        .ok_or(Error::TooLarge)?;

    let status = unsafe {
        RegSetValueExW(
            key,
            name.as_ptr(),
            0,
            kind,
            data.as_ptr().cast::<u8>(),
            bytes,
        )
    };
    if status == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(Error::Write { code: status })
    }
}

fn delete_value(key: HKEY, name: &[u16]) -> Result<(), Error> {
    let status = unsafe { RegDeleteValueW(key, name.as_ptr()) };
    match status {
        ERROR_SUCCESS | ERROR_FILE_NOT_FOUND => Ok(()),
        other => Err(Error::Delete { code: other }),
    }
}

/// Ask Explorer (and every other top-level window) to re-read the environment.
///
/// The value is already on disk; this only saves the user from having to log
/// out before a new terminal sees it. Failure is ignored on purpose.
fn broadcast_environment_change() {
    let message = wide("Environment");
    let mut result = 0usize;
    unsafe {
        SendMessageTimeoutW(
            HWND_BROADCAST,
            WM_SETTINGCHANGE,
            0,
            message.as_ptr() as isize,
            SMTO_ABORTIFHUNG,
            5000,
            &mut result,
        );
    }
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_key_path(label: &str) -> String {
        format!(
            r"Software\VibexInstallerEnvTest-{label}-{}",
            std::process::id()
        )
    }

    fn long_path() -> String {
        (0..100)
            .map(|index| format!(r"C:\tools\entry-{index:03}-with-a-long-name"))
            .collect::<Vec<_>>()
            .join(";")
    }

    fn delete_test_key(key_path: &str) {
        unsafe {
            windows_sys::Win32::System::Registry::RegDeleteKeyW(
                HKEY_CURRENT_USER,
                wide(key_path).as_ptr(),
            );
        }
    }

    #[test]
    fn a_long_expandable_value_is_edited_and_keeps_its_type() {
        let key_path = test_key_path("long");
        let value_name = "Path";
        let original = long_path();
        assert!(original.len() > 1500);

        {
            let key = open_key(&key_path).expect("create the test key");
            let name = wide(value_name);
            write_value(key.0, &name, &original, REG_EXPAND_SZ).expect("write the long test value");

            assert!(
                add_to(&key_path, value_name, r"C:\Vibex").expect("append to the long value"),
                "a new directory changes the value"
            );
            let value = read_value(key.0, &name)
                .expect("read the value back")
                .expect("the value exists");
            assert_eq!(
                value.kind, REG_EXPAND_SZ,
                "the expandable type must survive"
            );
            assert_eq!(value.text, format!(r"{original};C:\Vibex"));

            assert!(
                remove_from(&key_path, value_name, r"C:\Vibex").expect("remove the directory"),
                "the directory is present"
            );
            let value = read_value(key.0, &name)
                .expect("read the value back")
                .expect("the value exists");
            assert_eq!(value.kind, REG_EXPAND_SZ);
            assert_eq!(value.text, original, "every original entry must survive");
        }

        delete_test_key(&key_path);
    }

    #[test]
    fn a_reg_sz_value_stays_a_reg_sz_value() {
        let key_path = test_key_path("reg-sz");
        let value_name = "Path";

        {
            let key = open_key(&key_path).expect("create the test key");
            let name = wide(value_name);
            write_value(key.0, &name, r"C:\Tools", REG_SZ).expect("write the test value");

            assert!(add_to(&key_path, value_name, r"C:\Vibex").expect("append"));
            let value = read_value(key.0, &name)
                .expect("read")
                .expect("the value exists");
            assert_eq!(
                value.kind, REG_SZ,
                "a REG_SZ Path must not become REG_EXPAND_SZ"
            );
            assert_eq!(value.text, r"C:\Tools;C:\Vibex");

            remove_from(&key_path, value_name, r"C:\Vibex").expect("remove");
            assert!(
                add_to(&key_path, value_name, r"C:\Vibex").expect("the first append changes it")
            );
            assert!(
                !add_to(&key_path, value_name, r"C:\Vibex").expect("append twice"),
                "an existing entry must not be appended twice"
            );
        }

        delete_test_key(&key_path);
    }

    #[test]
    fn a_missing_value_is_created() {
        let key_path = test_key_path("missing");
        let value_name = "Path";

        {
            let key = open_key(&key_path).expect("create the test key");
            let name = wide(value_name);

            assert!(
                add_to(&key_path, value_name, r"C:\Vibex").expect("create the value"),
                "creating the value is a change"
            );
            let value = read_value(key.0, &name)
                .expect("read")
                .expect("the value exists");
            assert_eq!(value.text, r"C:\Vibex");
            assert_eq!(value.kind, REG_EXPAND_SZ);
        }

        delete_test_key(&key_path);
    }

    #[test]
    fn removing_the_last_entry_deletes_the_value() {
        let key_path = test_key_path("only");
        let value_name = "Path";

        {
            let key = open_key(&key_path).expect("create the test key");
            let name = wide(value_name);
            write_value(key.0, &name, r"C:\Vibex", REG_EXPAND_SZ).expect("write the test value");

            assert!(remove_from(&key_path, value_name, r"C:\Vibex").expect("remove"));
            assert!(
                read_value(key.0, &name).expect("read").is_none(),
                "the last entry must delete the value instead of leaving it empty"
            );
        }

        delete_test_key(&key_path);
    }
}
