//! Windows specifics: staying out of the backend's way, surviving the restart
//! that an acceleration change requires, and reporting failures without a
//! console to print to.

use std::path::Path;

/// Windows has no single "is this dark" rule, so use perceived luminance and
/// pick the title text to match.
pub fn is_dark(red: u8, green: u8, blue: u8) -> bool {
    let luminance = 0.299 * f32::from(red) + 0.587 * f32::from(green) + 0.114 * f32::from(blue);
    luminance < 128.0
}

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;
    use std::path::Path;
    use windows::core::HSTRING;
    use windows::Win32::Foundation::{CloseHandle, HWND, WAIT_OBJECT_0};
    use windows::Win32::Graphics::Dwm::{
        DwmSetWindowAttribute, DWMWA_BORDER_COLOR, DWMWA_CAPTION_COLOR, DWMWA_TEXT_COLOR,
        DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWINDOWATTRIBUTE,
    };
    use windows::Win32::System::Threading::{
        GetCurrentProcess, OpenProcess, SetPriorityClass, WaitForSingleObject,
        BELOW_NORMAL_PRIORITY_CLASS, PROCESS_SYNCHRONIZE,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        MessageBoxW, MB_ICONERROR, MB_ICONINFORMATION, MB_OK,
    };

    use super::is_dark;

    /// Windows' own sentinel for "use the default", not a colour.
    const DWMWA_COLOR_DEFAULT: u32 = 0xFFFF_FFFF;

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

    /// Paint the title bar, or hand it back to Windows with `None`.
    ///
    /// The caption/border/text attributes need Windows 11 (build 22000); on
    /// Windows 10 they fail harmlessly and only the dark-mode flag applies,
    /// which still darkens the caption there. Every call is best-effort.
    pub fn set_titlebar(hwnd: isize, colour: Option<(u8, u8, u8)>) {
        let window = HWND(hwnd as *mut c_void);

        let set = |attribute: DWMWINDOWATTRIBUTE, value: u32| unsafe {
            let _ = DwmSetWindowAttribute(
                window,
                attribute,
                std::ptr::from_ref(&value).cast(),
                size_of::<u32>() as u32,
            );
        };

        match colour {
            Some((red, green, blue)) => {
                let dark = is_dark(red, green, blue);
                // Also drives the button glyphs and the inactive caption,
                // which the colour attributes do not reach.
                set(DWMWA_USE_IMMERSIVE_DARK_MODE, u32::from(dark));
                set(DWMWA_CAPTION_COLOR, colorref(red, green, blue));
                set(DWMWA_BORDER_COLOR, colorref(red, green, blue));
                // Keep the title legible whichever way the page went.
                set(
                    DWMWA_TEXT_COLOR,
                    if dark {
                        colorref(0xF2, 0xF4, 0xF8)
                    } else {
                        colorref(0x11, 0x13, 0x17)
                    },
                );
            }
            None => {
                set(DWMWA_USE_IMMERSIVE_DARK_MODE, 0);
                for attribute in [DWMWA_CAPTION_COLOR, DWMWA_BORDER_COLOR, DWMWA_TEXT_COLOR] {
                    set(attribute, DWMWA_COLOR_DEFAULT);
                }
            }
        }
    }

    /// Windows wants 0x00BBGGRR, not the RGB order everything else uses.
    fn colorref(red: u8, green: u8, blue: u8) -> u32 {
        u32::from(red) | (u32::from(green) << 8) | (u32::from(blue) << 16)
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

    pub fn set_titlebar(_hwnd: isize, _colour: Option<(u8, u8, u8)>) {}
}

pub use imp::{set_low_priority, set_titlebar, show_error, show_info, wait_for_process_exit};

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
