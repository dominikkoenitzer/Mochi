//! The calls into Windows: dialogs, the registry, the Start menu shortcut and
//! the processes. Every decision about what to call them with is in plan.rs.

use std::ffi::c_void;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, ERROR_FILE_NOT_FOUND, GetLastError, HANDLE, LPARAM,
    WIN32_ERROR, WPARAM,
};
use windows::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
    CoTaskMemFree, IPersistFile,
};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_READ, REG_DWORD, REG_EXPAND_SZ, REG_SZ, REG_VALUE_TYPE,
    RRF_NOEXPAND, RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ, RegCloseKey, RegDeleteKeyValueW,
    RegDeleteTreeW, RegGetValueW, RegOpenKeyExW, RegQueryInfoKeyW, RegSetKeyValueW,
};
use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows::Win32::System::Threading::{
    CreateMutexW, GetCurrentProcess, GetCurrentProcessId, OpenProcessToken,
};
use windows::Win32::UI::Controls::{
    TASKDIALOG_BUTTON, TASKDIALOGCONFIG, TDCBF_CLOSE_BUTTON, TaskDialogIndirect,
};
use windows::Win32::UI::Shell::{
    FOLDERID_Programs, IShellLinkW, KF_FLAG_DEFAULT, SHGetKnownFolderPath, ShellLink,
};
use windows::Win32::UI::WindowsAndMessaging::{
    HWND_BROADCAST, IDOK, MB_ICONERROR, MB_ICONINFORMATION, MB_OKCANCEL, MessageBoxW,
    SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_SETTINGCHANGE,
};
use windows_core::{HSTRING, Interface, PCWSTR};

use crate::plan::RegValue;

/// Console programs started from here would each open a console window.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const TITLE: &str = "Mochi";
const ID_ACCEPT: i32 = 100;

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

/// A question with one button that does it and one that does not.
pub fn ask(instruction: &str, content: &str, accept: &str) -> bool {
    let title = wide(TITLE);
    let instruction_w = wide(instruction);
    let content_w = wide(content);
    let accept_w = wide(accept);
    let cancel_w = wide("Cancel");
    let buttons = [
        TASKDIALOG_BUTTON {
            nButtonID: ID_ACCEPT,
            pszButtonText: PCWSTR(accept_w.as_ptr()),
        },
        TASKDIALOG_BUTTON {
            nButtonID: 2,
            pszButtonText: PCWSTR(cancel_w.as_ptr()),
        },
    ];
    let config = TASKDIALOGCONFIG {
        cbSize: size_of::<TASKDIALOGCONFIG>() as u32,
        pszWindowTitle: PCWSTR(title.as_ptr()),
        pszMainInstruction: PCWSTR(instruction_w.as_ptr()),
        pszContent: PCWSTR(content_w.as_ptr()),
        cButtons: buttons.len() as u32,
        pButtons: buttons.as_ptr(),
        nDefaultButton: ID_ACCEPT,
        ..Default::default()
    };
    let mut pressed = 0;
    match unsafe { TaskDialogIndirect(&config, Some(&mut pressed), None, None) } {
        Ok(()) => pressed == ID_ACCEPT,
        Err(_) => {
            message_box(
                &format!("{instruction}\n\n{content}"),
                MB_OKCANCEL.0 | MB_ICONINFORMATION.0,
            ) == IDOK.0
        }
    }
}

/// A message with a Close button.
pub fn tell(instruction: &str, content: &str, error: bool) {
    let title = wide(TITLE);
    let instruction_w = wide(instruction);
    let content_w = wide(content);
    let config = TASKDIALOGCONFIG {
        cbSize: size_of::<TASKDIALOGCONFIG>() as u32,
        pszWindowTitle: PCWSTR(title.as_ptr()),
        pszMainInstruction: PCWSTR(instruction_w.as_ptr()),
        pszContent: PCWSTR(content_w.as_ptr()),
        dwCommonButtons: TDCBF_CLOSE_BUTTON,
        ..Default::default()
    };
    if unsafe { TaskDialogIndirect(&config, None, None, None) }.is_err() {
        let icon = if error {
            MB_ICONERROR.0
        } else {
            MB_ICONINFORMATION.0
        };
        message_box(&format!("{instruction}\n\n{content}"), icon);
    }
}

fn message_box(text: &str, style: u32) -> i32 {
    let text = HSTRING::from(text);
    let title = HSTRING::from(TITLE);
    unsafe {
        MessageBoxW(
            None,
            &text,
            &title,
            windows::Win32::UI::WindowsAndMessaging::MESSAGEBOX_STYLE(style),
        )
    }
    .0
}

/// The Start menu Programs folder, as Explorer knows it.
pub fn start_menu_programs() -> Option<PathBuf> {
    unsafe {
        let found = SHGetKnownFolderPath(&FOLDERID_Programs, KF_FLAG_DEFAULT, None).ok()?;
        let path = found.to_string().ok();
        CoTaskMemFree(Some(found.0 as *const c_void));
        path.map(PathBuf::from)
    }
}

/// By the token: an elevated process hands its rights to every child.
pub fn is_elevated() -> bool {
    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }
        let mut elevation = TOKEN_ELEVATION::default();
        let mut length = 0;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            Some(&mut elevation as *mut _ as *mut c_void),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut length,
        )
        .is_ok();
        let _ = CloseHandle(token);
        ok && elevation.TokenIsElevated != 0
    }
}

/// Whether a mochi.exe runs in this logon session.
pub fn mochi_running() -> bool {
    running(&["mochi.exe"])
}

/// Whether a mochi.exe or a mochic.exe runs in this logon session. Either one
/// holds its file open, so neither can be replaced while it runs.
pub fn any_mochi_running() -> bool {
    running(&["mochi.exe", "mochic.exe"])
}

fn running(names: &[&str]) -> bool {
    unsafe {
        let mut session = 0;
        if ProcessIdToSessionId(GetCurrentProcessId(), &mut session).is_err() {
            return false;
        }
        let Ok(snapshot) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
            return false;
        };
        let mut entry = PROCESSENTRY32W {
            dwSize: size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut found = false;
        let mut more = Process32FirstW(snapshot, &mut entry).is_ok();
        while more {
            let len = entry
                .szExeFile
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(entry.szExeFile.len());
            let name = String::from_utf16_lossy(&entry.szExeFile[..len]);
            if names.iter().any(|wanted| name.eq_ignore_ascii_case(wanted)) {
                let mut theirs = 0;
                if ProcessIdToSessionId(entry.th32ProcessID, &mut theirs).is_ok()
                    && theirs == session
                {
                    found = true;
                    break;
                }
            }
            more = Process32NextW(snapshot, &mut entry).is_ok();
        }
        let _ = CloseHandle(snapshot);
        found
    }
}

/// Polls until `done` holds or `limit` passes.
pub fn wait_for(limit: Duration, done: impl Fn() -> bool) -> bool {
    let start = Instant::now();
    loop {
        if done() {
            return true;
        }
        if start.elapsed() >= limit {
            return false;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Runs a console program without a window and returns its exit code and
/// whatever it said.
///
/// What it says goes through a file, not a pipe. `mochic start` launches the
/// daemon with a copy of every handle mochic inherited, so a pipe would not
/// close until Mochi exits and the setup would wait for it forever. The file
/// is opened shareable for deletion, so it goes away even while Mochi still
/// holds that copy.
///
/// It runs in its own folder, as the Start menu entry does: the daemon that
/// `mochic start` launches inherits the folder and keeps it for its whole
/// life, and the folder this setup was opened from may be one the user wants
/// to delete.
pub fn run_hidden(program: &Path, args: &[&str]) -> std::io::Result<(i32, String)> {
    let mut command = Command::new(program);
    if let Some(dir) = program.parent().filter(|dir| dir.is_dir()) {
        command.current_dir(dir);
    }
    static RUNS: AtomicU32 = AtomicU32::new(0);
    let said = std::env::temp_dir().join(format!(
        "mochi-setup-{}-{}.txt",
        std::process::id(),
        RUNS.fetch_add(1, Ordering::Relaxed)
    ));
    let file = std::fs::File::create(&said)?;
    let status = file.try_clone().and_then(|stdout| {
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(stdout)
            .stderr(file)
            .creation_flags(CREATE_NO_WINDOW)
            .status()
    });
    let text = std::fs::read(&said).unwrap_or_default();
    let _ = std::fs::remove_file(&said);
    let status = status?;
    Ok((
        status.code().unwrap_or(-1),
        String::from_utf8_lossy(&text).trim().to_owned(),
    ))
}

/// Starts cmd.exe without a window and without waiting for it. It works in
/// System32 rather than in this process's folder: a folder that is some
/// process's working directory cannot be removed.
pub fn spawn_cmd(raw_args: &str) -> std::io::Result<()> {
    let cmd = system32().join("cmd.exe");
    Command::new(cmd)
        .raw_arg(raw_args)
        .current_dir(system32())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map(drop)
}

/// Opens a file the way a double click in Explorer would. Explorer runs at
/// the user's normal rights whatever this process runs at.
pub fn open_with_explorer(path: &Path) -> std::io::Result<()> {
    let windir =
        std::env::var_os("SystemRoot").map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from);
    Command::new(windir.join("explorer.exe"))
        .arg(path)
        .spawn()
        .map(drop)
}

pub fn system32() -> PathBuf {
    std::env::var_os("SystemRoot")
        .map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from)
        .join("System32")
}

fn check(status: WIN32_ERROR, what: &str) -> Result<(), String> {
    if status.is_ok() {
        Ok(())
    } else {
        Err(format!(
            "{what}: {}",
            windows_core::Error::from(status.to_hresult()).message()
        ))
    }
}

/// A string value as it is stored, %VARIABLES% unexpanded, with whether it
/// is REG_EXPAND_SZ. None when there is no such value.
pub fn read_text(key: &str, name: &str) -> Result<Option<(String, bool)>, String> {
    let key_w = HSTRING::from(key);
    let name_w = HSTRING::from(name);
    let flags = RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ | RRF_NOEXPAND;
    let mut kind = REG_VALUE_TYPE::default();
    let mut size = 0u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            &key_w,
            &name_w,
            flags,
            Some(&mut kind),
            None,
            Some(&mut size),
        )
    };
    if status == ERROR_FILE_NOT_FOUND || status == windows::Win32::Foundation::ERROR_PATH_NOT_FOUND
    {
        return Ok(None);
    }
    check(status, &format!("reading {key}\\{name}"))?;
    let mut buffer = vec![0u16; (size as usize).div_ceil(2) + 1];
    let mut size = (buffer.len() * 2) as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            &key_w,
            &name_w,
            flags,
            Some(&mut kind),
            Some(buffer.as_mut_ptr().cast()),
            Some(&mut size),
        )
    };
    check(status, &format!("reading {key}\\{name}"))?;
    let len = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    Ok(Some((
        String::from_utf16_lossy(&buffer[..len]),
        kind == REG_EXPAND_SZ,
    )))
}

pub fn write_text(key: &str, name: &str, value: &str, expand: bool) -> Result<(), String> {
    let data = wide(value);
    let kind = if expand { REG_EXPAND_SZ } else { REG_SZ };
    let status = unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            &HSTRING::from(key),
            &HSTRING::from(name),
            kind.0,
            Some(data.as_ptr().cast()),
            (data.len() * 2) as u32,
        )
    };
    check(status, &format!("writing {key}\\{name}"))
}

pub fn write_value(key: &str, name: &str, value: &RegValue) -> Result<(), String> {
    match value {
        RegValue::Text(text) => write_text(key, name, text, false),
        RegValue::Number(number) => {
            let status = unsafe {
                RegSetKeyValueW(
                    HKEY_CURRENT_USER,
                    &HSTRING::from(key),
                    &HSTRING::from(name),
                    REG_DWORD.0,
                    Some((number as *const u32).cast()),
                    4,
                )
            };
            check(status, &format!("writing {key}\\{name}"))
        }
    }
}

/// Removes a value; one that is not there is not an error.
pub fn delete_value(key: &str, name: &str) -> Result<(), String> {
    let status =
        unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, &HSTRING::from(key), &HSTRING::from(name)) };
    if status == ERROR_FILE_NOT_FOUND || status == windows::Win32::Foundation::ERROR_PATH_NOT_FOUND
    {
        return Ok(());
    }
    check(status, &format!("removing {key}\\{name}"))
}

/// Removes a key and everything under it; one that is not there is fine.
/// Given a subkey name, RegDeleteTreeW removes the key itself as well.
pub fn delete_key(key: &str) -> Result<(), String> {
    let status = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, &HSTRING::from(key)) };
    if status == ERROR_FILE_NOT_FOUND || status == windows::Win32::Foundation::ERROR_PATH_NOT_FOUND
    {
        return Ok(());
    }
    check(status, &format!("removing {key}"))
}

/// Whether a key exists and holds neither values nor subkeys.
pub fn key_is_empty(key: &str) -> bool {
    unsafe {
        let mut handle = HKEY::default();
        if RegOpenKeyExW(
            HKEY_CURRENT_USER,
            &HSTRING::from(key),
            None,
            KEY_READ,
            &mut handle,
        )
        .is_err()
        {
            return false;
        }
        let mut subkeys = 0;
        let mut values = 0;
        let status = RegQueryInfoKeyW(
            handle,
            None,
            None,
            None,
            Some(&mut subkeys),
            None,
            None,
            Some(&mut values),
            None,
            None,
            None,
            None,
        );
        let _ = RegCloseKey(handle);
        status.is_ok() && subkeys == 0 && values == 0
    }
}

/// Tells Explorer and every program started after it that the user
/// environment changed. A registry write alone does not, so the new PATH
/// would only be seen after the next logon.
pub fn announce_environment_change() {
    let environment = wide("Environment");
    let mut result = 0usize;
    unsafe {
        SendMessageTimeoutW(
            HWND_BROADCAST,
            WM_SETTINGCHANGE,
            WPARAM(0),
            LPARAM(environment.as_ptr() as isize),
            SMTO_ABORTIFHUNG,
            5000,
            Some(&mut result),
        );
    }
}

/// The Start menu entry. It runs mochic through a headless console host:
/// a shortcut to mochic.exe itself would open a terminal window on every
/// click, because mochic is a console program.
pub fn create_shortcut(path: &Path, mochic: &Path, icon: &Path) -> Result<(), String> {
    let fail = |e: windows_core::Error| format!("creating {}: {}", path.display(), e.message());
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let link: IShellLinkW =
            CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).map_err(fail)?;
        let host = system32().join("conhost.exe");
        link.SetPath(&HSTRING::from(host.as_os_str()))
            .map_err(fail)?;
        link.SetArguments(&HSTRING::from(format!(
            "--headless \"{}\" start",
            mochic.display()
        )))
        .map_err(fail)?;
        if let Some(dir) = mochic.parent() {
            link.SetWorkingDirectory(&HSTRING::from(dir.as_os_str()))
                .map_err(fail)?;
        }
        link.SetIconLocation(&HSTRING::from(icon.as_os_str()), 0)
            .map_err(fail)?;
        link.SetDescription(&HSTRING::from("Start Mochi"))
            .map_err(fail)?;
        let file: IPersistFile = link.cast().map_err(fail)?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
        }
        file.Save(&HSTRING::from(path.as_os_str()), true)
            .map_err(fail)
    }
}

/// Whether this is the only setup running for this user. A second double
/// click while the first is still busy would race it for the same files.
pub fn first_instance() -> bool {
    unsafe {
        match CreateMutexW(None, true, &HSTRING::from(r"Local\MochiSetup")) {
            // The handle stays open until this process exits, which is the point.
            Ok(_) => GetLastError() != ERROR_ALREADY_EXISTS,
            Err(_) => true,
        }
    }
}
