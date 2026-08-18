//! Items appended to each window's system menu -- the one Windows shows when
//! you right-click the title bar, or press Alt+Space.
//!
//! Reusing the system menu rather than adding a menu bar keeps the frame free
//! of extra chrome and needs no menu toolkit: the whole thing is three
//! `AppendMenuW` calls and a window subclass.

/// The commands DiffusionFrame adds to the system menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuCommand {
    Refresh,
    ClearCacheAndRestart,
    ColourManagement,
    HardwareAcceleration,
}

// The ids and labels drive the Windows system menu; off Windows only the
// tests exercise them.
#[cfg_attr(not(windows), allow(dead_code))]
impl MenuCommand {
    /// Windows reserves the low four bits of `WM_SYSCOMMAND`'s `wParam` for
    /// its own use, so custom ids must be multiples of 16. They must also stay
    /// clear of the built-in `SC_*` commands, which all start at `0xF000`.
    const fn id(self) -> usize {
        match self {
            MenuCommand::Refresh => 0x0010,
            MenuCommand::ClearCacheAndRestart => 0x0020,
            MenuCommand::ColourManagement => 0x0030,
            MenuCommand::HardwareAcceleration => 0x0040,
        }
    }

    const fn from_id(id: usize) -> Option<Self> {
        match id {
            0x0010 => Some(MenuCommand::Refresh),
            0x0020 => Some(MenuCommand::ClearCacheAndRestart),
            0x0030 => Some(MenuCommand::ColourManagement),
            0x0040 => Some(MenuCommand::HardwareAcceleration),
            _ => None,
        }
    }

    /// Both toggles only take effect on a fresh WebView2 environment, so the
    /// label says so rather than leaving the restart as a surprise.
    fn label(self, enabled: bool) -> String {
        match self {
            MenuCommand::Refresh => "&Refresh page\tF5".to_string(),
            MenuCommand::ClearCacheAndRestart => "Clear cache and restart\t(restarts)".to_string(),
            MenuCommand::ColourManagement => {
                format!("{} Colour Management\t(restarts)", verb(enabled))
            }
            MenuCommand::HardwareAcceleration => {
                format!("{} Hardware Acceleration\tCtrl+Shift+G", verb(enabled))
            }
        }
    }
}

#[cfg_attr(not(windows), allow(dead_code))]
fn verb(enabled: bool) -> &'static str {
    if enabled {
        "Disable"
    } else {
        "Enable"
    }
}

#[cfg(windows)]
mod imp {
    use super::MenuCommand;
    use std::cell::RefCell;
    use std::ffi::c_void;

    use windows::core::{HSTRING, PCWSTR};
    use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
    use windows::Win32::UI::WindowsAndMessaging::{
        AppendMenuW, GetSystemMenu, MF_SEPARATOR, MF_STRING, WM_SYSCOMMAND,
    };

    const SUBCLASS_ID: usize = 0xDF01;

    /// Called with the command and the window whose menu was used.
    type Handler = Box<dyn Fn(MenuCommand, isize)>;

    thread_local! {
        /// Set once by the event loop. Everything here runs on the main
        /// thread, so a thread-local beats threading a pointer through
        /// `SetWindowSubclass`'s ref-data.
        static HANDLER: RefCell<Option<Handler>> = const { RefCell::new(None) };
    }

    pub fn set_handler(handler: impl Fn(MenuCommand, isize) + 'static) {
        HANDLER.with(|slot| *slot.borrow_mut() = Some(Box::new(handler)));
    }

    pub fn install(hwnd: isize, colour_management: bool, hardware_acceleration: bool) {
        let window = HWND(hwnd as *mut c_void);
        unsafe {
            // `false` returns the window's own copy of the menu, not the
            // shared default, so appending here affects only this window.
            let menu = GetSystemMenu(window, false);
            if menu.is_invalid() {
                return;
            }

            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
            for (command, enabled) in [
                (MenuCommand::Refresh, false),
                (MenuCommand::ClearCacheAndRestart, false),
                (MenuCommand::ColourManagement, colour_management),
                (MenuCommand::HardwareAcceleration, hardware_acceleration),
            ] {
                let label = HSTRING::from(command.label(enabled));
                let _ = AppendMenuW(menu, MF_STRING, command.id(), &label);
            }

            // Subclassing rather than a message hook: the menu's modal loop
            // sends WM_SYSCOMMAND straight to the window procedure, so it
            // never passes through the thread's message queue.
            let _ = SetWindowSubclass(window, Some(subclass_proc), SUBCLASS_ID, 0);
        }
    }

    unsafe extern "system" fn subclass_proc(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
        _subclass_id: usize,
        _ref_data: usize,
    ) -> LRESULT {
        if message == WM_SYSCOMMAND {
            // Mask off the four bits Windows uses internally.
            if let Some(command) = MenuCommand::from_id(wparam.0 & 0xFFF0) {
                HANDLER.with(|slot| {
                    if let Some(handler) = slot.borrow().as_ref() {
                        handler(command, hwnd.0 as isize);
                    }
                });
                return LRESULT(0);
            }
        }
        unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::MenuCommand;

    pub fn set_handler(_handler: impl Fn(MenuCommand, isize) + 'static) {}

    pub fn install(_hwnd: isize, _colour_management: bool, _hardware_acceleration: bool) {}
}

pub use imp::{install, set_handler};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_round_trip() {
        for command in [
            MenuCommand::Refresh,
            MenuCommand::ClearCacheAndRestart,
            MenuCommand::ColourManagement,
            MenuCommand::HardwareAcceleration,
        ] {
            assert_eq!(MenuCommand::from_id(command.id()), Some(command));
        }
    }

    #[test]
    fn ids_are_usable_as_wm_syscommand_parameters() {
        for command in [
            MenuCommand::Refresh,
            MenuCommand::ClearCacheAndRestart,
            MenuCommand::ColourManagement,
            MenuCommand::HardwareAcceleration,
        ] {
            // Multiples of 16, so masking with 0xFFF0 is lossless...
            assert_eq!(command.id() & 0xFFF0, command.id());
            // ...and clear of the built-in SC_* range.
            assert!(command.id() < 0xF000);
        }
    }

    #[test]
    fn unknown_ids_are_left_to_the_system() {
        // SC_CLOSE and friends must fall through to the default handler.
        assert_eq!(MenuCommand::from_id(0xF060), None);
        assert_eq!(MenuCommand::from_id(0), None);
    }

    #[test]
    fn toggle_labels_say_what_the_click_will_do() {
        assert!(MenuCommand::ColourManagement
            .label(true)
            .starts_with("Disable"));
        assert!(MenuCommand::ColourManagement
            .label(false)
            .starts_with("Enable"));
        // Both toggles need a restart; the label warns before the click.
        assert!(MenuCommand::ColourManagement
            .label(true)
            .contains("restarts"));
    }

    #[test]
    fn toggle_labels_use_title_case_for_the_setting_name() {
        assert!(MenuCommand::ColourManagement
            .label(true)
            .contains("Colour Management"));
        assert!(MenuCommand::HardwareAcceleration
            .label(true)
            .contains("Hardware Acceleration"));
    }

    #[test]
    fn clear_cache_warns_it_restarts_too() {
        assert!(MenuCommand::ClearCacheAndRestart
            .label(true)
            .contains("restarts"));
    }
}
