//! Windows specifics: staying out of the backend's way, surviving the restart
//! that an acceleration change requires, and reporting failures without a
//! console to print to.

use std::path::Path;

#[cfg(windows)]
mod imp {
    use std::path::Path;
    use windows::core::HSTRING;
    use windows::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
    use windows::Win32::System::Threading::{
        GetCurrentProcess, OpenProcess, SetPriorityClass, WaitForSingleObject,
        BELOW_NORMAL_PRIORITY_CLASS, PROCESS_SYNCHRONIZE,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        MessageBoxW, MB_ICONERROR, MB_ICONINFORMATION, MB_OK,
    };

    /// Below-normal keeps the frame responsive while guaranteeing it never
    /// preempts the sampler. Deliberately not "idle", which would make the UI
    /// stutter badly during generation.
    pub fn set_low_priority() {
        unsafe {
            let _ = SetPriorityClass(GetCurrentProcess(), BELOW_NORMAL_PRIORITY_CLASS);
        }
    }

    /// Block until `pid` exits, so a restarting instance does not race the
    /// outgoing one for the WebView2 user-data folder. WebView2 refuses to
    /// create an environment while another process holds that folder with
    /// different browser arguments, which is exactly what a GPU toggle changes.
    pub fn wait_for_process_exit(pid: u32, timeout_ms: u32) {
        unsafe {
            let Ok(handle) = OpenProcess(PROCESS_SYNCHRONIZE, false, pid) else {
                // Already gone, which is the outcome we were waiting for.
                return;
            };
            let _ = WaitForSingleObject(handle, timeout_ms) == WAIT_OBJECT_0;
            let _ = CloseHandle(handle);
        }
    }

    pub fn show_error(message: &str) {
        unsafe {
            MessageBoxW(
                None,
                &HSTRING::from(message),
                &HSTRING::from("DiffusionFrame"),
                MB_OK | MB_ICONERROR,
            );
        }
    }

    /// Release builds have no console, so `--help` needs a window. Printing as
    /// well costs nothing and is what anyone running this from a shell wants.
    pub fn show_info(message: &str) {
        println!("{message}");
        unsafe {
            MessageBoxW(
                None,
                &HSTRING::from(message),
                &HSTRING::from("DiffusionFrame"),
                MB_OK | MB_ICONINFORMATION,
            );
        }
    }

    pub fn reveal_in_file_manager(path: &Path) {
        let _ = std::process::Command::new("explorer.exe").arg(path).spawn();
    }
}

#[cfg(not(windows))]
mod imp {
    use std::path::Path;

    pub fn set_low_priority() {}

    pub fn wait_for_process_exit(_pid: u32, _timeout_ms: u32) {}

    pub fn show_error(message: &str) {
        eprintln!("DiffusionFrame: {message}");
    }

    pub fn show_info(message: &str) {
        println!("{message}");
    }

    pub fn reveal_in_file_manager(path: &Path) {
        let _ = std::process::Command::new("xdg-open").arg(path).spawn();
    }
}

pub use imp::{set_low_priority, show_error, show_info, wait_for_process_exit};

pub fn reveal_in_file_manager(path: &Path) {
    imp::reveal_in_file_manager(path);
}

/// Restart in place, handing the new process our PID so it can wait for the
/// WebView2 profile lock to be released before building its own environment.
///
/// A command-line address is carried across, since it lives only in argv and
/// would otherwise be lost on the first acceleration toggle.
pub fn restart(override_url: Option<&str>) -> ! {
    if let Ok(exe) = std::env::current_exe() {
        let mut command = std::process::Command::new(exe);
        command
            .arg("--await-exit")
            .arg(std::process::id().to_string());
        if let Some(url) = override_url {
            command.arg("--url").arg(url);
        }
        let _ = command.spawn();
    }
    std::process::exit(0);
}
