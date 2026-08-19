//! A deliberately small flat-file config.
//!
//! The format is `key = value`, one per line, `#` starts a comment. It is
//! hand-parsed so that DiffusionFrame does not pull a serialization stack in
//! just to read a dozen settings.

use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub name: String,
    pub url: String,
}

/// What colour the window's title bar should be painted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Titlebar {
    /// Black, matching the dark UIs these backends ship with.
    #[default]
    Black,
    /// Follow the background colour of whatever page is on screen.
    Page,
    /// Leave it to Windows.
    System,
}

impl Titlebar {
    fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "black" => Some(Titlebar::Black),
            "page" => Some(Titlebar::Page),
            "system" => Some(Titlebar::System),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Titlebar::Black => "black",
            Titlebar::Page => "page",
            Titlebar::System => "system",
        }
    }

    /// The colour to paint before the page has said anything, if any.
    pub fn initial_colour(self) -> Option<(u8, u8, u8)> {
        match self {
            // Page-following starts black too, so the caption never flashes
            // the system colour while the page is still loading.
            Titlebar::Black | Titlebar::Page => Some((0, 0, 0)),
            Titlebar::System => None,
        }
    }

    /// Whether the page needs to report its background colour.
    pub fn follows_page(self) -> bool {
        self == Titlebar::Page
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowState {
    pub width: u32,
    pub height: u32,
    pub position: Option<(i32, i32)>,
    pub maximized: bool,
}

impl Default for WindowState {
    fn default() -> Self {
        Self {
            width: 1440,
            height: 900,
            position: None,
            maximized: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    /// When false the webview is started with the GPU disabled, leaving all
    /// video memory to the diffusion backend.
    pub hardware_acceleration: bool,
    /// When false the webview stops converting page colours into the
    /// display's ICC profile, so images reach the screen unaltered.
    pub colour_management: bool,
    /// Run the frame below normal scheduler priority so it yields to the
    /// backend under load.
    pub low_priority: bool,
    /// Release renderer memory and stop compositing while minimized.
    pub idle_throttle: bool,
    pub titlebar: Titlebar,
    pub zoom: f64,
    pub active: usize,
    pub window: WindowState,
    pub targets: Vec<Target>,
    /// A backend given on the command line. Takes precedence over `active` for
    /// this run and is never written back to the file.
    pub override_target: Option<Target>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            hardware_acceleration: true,
            colour_management: true,
            low_priority: true,
            idle_throttle: true,
            titlebar: Titlebar::default(),
            zoom: 1.0,
            active: 0,
            window: WindowState::default(),
            targets: default_targets(),
            override_target: None,
        }
    }
}

fn default_targets() -> Vec<Target> {
    [
        ("ComfyUI", "http://127.0.0.1:8188"),
        ("A1111 WebUI", "http://127.0.0.1:7860"),
        ("Forge", "http://127.0.0.1:7861"),
        ("SwarmUI", "http://127.0.0.1:7801"),
        ("InvokeAI", "http://127.0.0.1:9090"),
    ]
    .iter()
    .map(|(name, url)| Target {
        name: (*name).to_string(),
        url: (*url).to_string(),
    })
    .collect()
}

impl Config {
    pub fn active_target(&self) -> &Target {
        // `active` is clamped on load, but targets can never be empty either
        // way, so fall back to the first entry rather than panicking.
        self.override_target
            .as_ref()
            .unwrap_or_else(|| self.targets.get(self.active).unwrap_or(&self.targets[0]))
    }

    /// Which configured backend to leave out of the offline page's switch
    /// list. A command-line address excludes nothing, since it is not one of
    /// the configured entries.
    pub fn switch_list_exclusion(&self) -> Option<usize> {
        self.override_target.is_none().then_some(self.active)
    }

    /// Point the frame at a command-line address, adopting a configured
    /// backend when the address matches one.
    pub fn apply_override(&mut self, url: &str) {
        match crate::cli::resolve(url, &self.targets) {
            Ok(index) => {
                self.active = index;
                self.override_target = None;
            }
            Err(target) => self.override_target = Some(target),
        }
    }

    /// Load the config, writing a commented default file on first run.
    pub fn load() -> Self {
        let path = config_path();
        match fs::read_to_string(&path) {
            Ok(text) => Config::parse(&text),
            Err(_) => {
                let config = Config::default();
                config.save();
                config
            }
        }
    }

    /// Unknown keys are ignored and malformed values fall back to their
    /// defaults, so a hand-edited file can never keep the app from starting.
    pub fn parse(text: &str) -> Self {
        let mut config = Config {
            targets: Vec::new(),
            ..Config::default()
        };

        for line in text.lines() {
            let line = match line.split_once('#') {
                Some((before, _)) => before,
                None => line,
            }
            .trim();

            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let (key, value) = (key.trim(), value.trim());

            match key {
                "hardware_acceleration" => config.hardware_acceleration = parse_bool(value, true),
                "colour_management" => config.colour_management = parse_bool(value, true),
                "low_priority" => config.low_priority = parse_bool(value, true),
                "idle_throttle" => config.idle_throttle = parse_bool(value, true),
                "titlebar" => config.titlebar = Titlebar::parse(value).unwrap_or_default(),
                "zoom" => config.zoom = value.parse().unwrap_or(1.0),
                "active" => config.active = value.parse().unwrap_or(0),
                "window_width" => config.window.width = value.parse().unwrap_or(1440),
                "window_height" => config.window.height = value.parse().unwrap_or(900),
                "window_maximized" => config.window.maximized = parse_bool(value, false),
                "window_position" => {
                    config.window.position = value
                        .split_once(',')
                        .and_then(|(x, y)| Some((x.trim().parse().ok()?, y.trim().parse().ok()?)))
                }
                "target" => {
                    if let Some((name, url)) = value.split_once('|') {
                        let (name, url) = (name.trim(), url.trim());
                        if !name.is_empty() && !url.is_empty() {
                            config.targets.push(Target {
                                name: name.to_string(),
                                url: url.to_string(),
                            });
                        }
                    }
                }
                _ => {}
            }
        }

        if config.targets.is_empty() {
            config.targets = default_targets();
        }
        if config.active >= config.targets.len() {
            config.active = 0;
        }
        config.zoom = config.zoom.clamp(0.25, 4.0);
        config.window.width = config.window.width.clamp(320, 16_384);
        config.window.height = config.window.height.clamp(240, 16_384);
        config
    }

    pub fn save(&self) {
        let path = config_path();
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let _ = fs::write(&path, self.serialize());
    }

    pub fn serialize(&self) -> String {
        let mut out = String::new();
        out.push_str("# DiffusionFrame configuration\n");
        out.push_str("# Rewritten on exit -- comments you add here are not preserved.\n\n");

        out.push_str(
            "# Give the GPU entirely to the diffusion backend by setting this to false.\n",
        );
        out.push_str(
            "# Costs some UI smoothness on large graphs. Toggle live with Ctrl+Shift+G.\n",
        );
        out.push_str(&format!(
            "hardware_acceleration = {}\n\n",
            self.hardware_acceleration
        ));

        out.push_str("# Convert page colours into the display's ICC profile. Set false on a\n");
        out.push_str("# wide-gamut monitor to see generated images with their original values.\n");
        out.push_str(&format!(
            "colour_management = {}\n\n",
            self.colour_management
        ));

        out.push_str("# Run below normal scheduler priority so sampling never waits on the UI.\n");
        out.push_str(&format!("low_priority = {}\n\n", self.low_priority));

        out.push_str("# Release renderer memory and stop compositing while minimized.\n");
        out.push_str(&format!("idle_throttle = {}\n\n", self.idle_throttle));

        out.push_str("# Title bar colour: black, page (follow the page background), or system.\n");
        out.push_str(&format!("titlebar = {}\n\n", self.titlebar.as_str()));

        out.push_str(&format!("zoom = {}\n", self.zoom));
        out.push_str(&format!("active = {}\n\n", self.active));

        out.push_str(&format!("window_width = {}\n", self.window.width));
        out.push_str(&format!("window_height = {}\n", self.window.height));
        if let Some((x, y)) = self.window.position {
            out.push_str(&format!("window_position = {x}, {y}\n"));
        }
        out.push_str(&format!("window_maximized = {}\n\n", self.window.maximized));

        out.push_str("# Backends, as `target = Name | URL`. Switch with Ctrl+Shift+1..9.\n");
        for target in &self.targets {
            out.push_str(&format!("target = {} | {}\n", target.name, target.url));
        }

        out
    }
}

fn parse_bool(value: &str, fallback: bool) -> bool {
    match value.to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" | "1" => true,
        "false" | "no" | "off" | "0" => false,
        _ => fallback,
    }
}

/// A `data` folder next to the executable opts into portable mode: config and
/// browser storage both move there instead of `%APPDATA%`. Never created
/// automatically -- only ever activated by a folder the user made themselves,
/// so an existing install's behavior can't change out from under it.
fn portable_data_dir() -> Option<PathBuf> {
    portable_data_dir_for(std::env::current_exe().ok()?)
}

fn portable_data_dir_for(exe_path: PathBuf) -> Option<PathBuf> {
    let candidate = exe_path.parent()?.join("data");
    candidate.is_dir().then_some(candidate)
}

pub fn config_dir() -> PathBuf {
    if let Some(portable) = portable_data_dir() {
        return portable;
    }

    #[cfg(windows)]
    let base = std::env::var_os("APPDATA").map(PathBuf::from);
    #[cfg(not(windows))]
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")));

    base.unwrap_or_else(|| PathBuf::from("."))
        .join("DiffusionFrame")
}

pub fn config_path() -> PathBuf {
    config_dir().join("config.txt")
}

/// WebView2 keeps its profile here. Shared across acceleration modes so that
/// toggling the GPU never discards saved workflows or UI settings.
pub fn webview_data_dir() -> PathBuf {
    config_dir().join("webview2")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch directory unique to the calling test, so parallel test runs
    /// never race on the same path.
    fn scratch_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("diffusionframe-test-{name}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_data_folder_next_to_the_exe_is_detected() {
        let root = scratch_dir("portable-yes");
        fs::create_dir_all(root.join("data")).unwrap();
        let exe = root.join("diffusionframe.exe");

        assert_eq!(portable_data_dir_for(exe), Some(root.join("data")));
    }

    #[test]
    fn no_data_folder_means_no_portable_mode() {
        let root = scratch_dir("portable-no");
        let exe = root.join("diffusionframe.exe");

        assert_eq!(portable_data_dir_for(exe), None);
    }

    #[test]
    fn a_file_named_data_does_not_opt_in() {
        // A stray same-named file next to the exe must not be mistaken for
        // the portable marker.
        let root = scratch_dir("portable-file-not-dir");
        fs::write(root.join("data"), b"not a directory").unwrap();
        let exe = root.join("diffusionframe.exe");

        assert_eq!(portable_data_dir_for(exe), None);
    }

    #[test]
    fn the_data_folder_is_relative_to_the_exe_not_the_working_directory() {
        let root = scratch_dir("portable-relative-to-exe");
        fs::create_dir_all(root.join("subdir")).unwrap();
        fs::create_dir_all(root.join("data")).unwrap();
        // A "data" folder next to the exe's *containing* directory, one level
        // up from where the exe actually lives, must not count.
        let exe = root.join("subdir").join("diffusionframe.exe");

        assert_eq!(portable_data_dir_for(exe), None);
    }

    #[test]
    fn settings_survive_a_save_load_round_trip() {
        let original = Config {
            hardware_acceleration: false,
            colour_management: false,
            low_priority: false,
            idle_throttle: false,
            titlebar: Titlebar::Page,
            zoom: 1.25,
            active: 2,
            window: WindowState {
                width: 1600,
                height: 1000,
                position: Some((-7, 42)),
                maximized: false,
            },
            targets: default_targets(),
            override_target: None,
        };

        let reloaded = Config::parse(&original.serialize());

        assert!(!reloaded.hardware_acceleration);
        assert!(!reloaded.colour_management);
        assert!(!reloaded.low_priority);
        assert!(!reloaded.idle_throttle);
        assert_eq!(reloaded.titlebar, Titlebar::Page);
        assert_eq!(reloaded.zoom, 1.25);
        assert_eq!(reloaded.active, 2);
        assert_eq!(reloaded.window, original.window);
        assert_eq!(reloaded.targets, original.targets);
    }

    #[test]
    fn comments_and_unknown_keys_are_ignored() {
        let config = Config::parse(
            "# a comment\n\
             zoom = 1.5  # trailing comment\n\
             something_new = 3\n\
             \n\
             target = Remote | http://192.168.1.20:8188\n",
        );

        assert_eq!(config.zoom, 1.5);
        assert_eq!(config.targets.len(), 1);
        assert_eq!(config.targets[0].name, "Remote");
        assert_eq!(config.targets[0].url, "http://192.168.1.20:8188");
    }

    #[test]
    fn a_broken_file_still_yields_a_usable_config() {
        let config = Config::parse(
            "hardware_acceleration = perhaps\n\
             zoom = lots\n\
             active = 99\n\
             window_width = 3\n\
             target = no separator here\n",
        );

        // Bad values fall back rather than aborting startup, and an out-of-range
        // `active` must not outlive the target list it indexes.
        assert!(config.hardware_acceleration);
        assert!(config.colour_management);
        assert_eq!(config.zoom, 1.0);
        assert_eq!(config.active, 0);
        assert_eq!(config.window.width, 320);
        assert_eq!(config.targets, default_targets());
    }

    #[test]
    fn titlebar_values_parse_and_unknown_ones_fall_back_to_black() {
        assert_eq!(Config::parse("titlebar = page").titlebar, Titlebar::Page);
        assert_eq!(
            Config::parse("titlebar = SYSTEM").titlebar,
            Titlebar::System
        );
        assert_eq!(Config::parse("titlebar = mauve").titlebar, Titlebar::Black);
        assert_eq!(Config::default().titlebar, Titlebar::Black);
    }

    #[test]
    fn only_page_following_needs_the_background_script() {
        assert!(Titlebar::Page.follows_page());
        assert!(!Titlebar::Black.follows_page());
        assert!(!Titlebar::System.follows_page());
        // Black and page both start black, so the caption never flashes.
        assert_eq!(Titlebar::Page.initial_colour(), Some((0, 0, 0)));
        assert_eq!(Titlebar::Black.initial_colour(), Some((0, 0, 0)));
        assert_eq!(Titlebar::System.initial_colour(), None);
    }

    #[test]
    fn zoom_is_clamped_to_a_legible_range() {
        assert_eq!(Config::parse("zoom = 99").zoom, 4.0);
        assert_eq!(Config::parse("zoom = 0.01").zoom, 0.25);
    }

    #[test]
    fn a_command_line_address_wins_over_the_configured_backend() {
        let mut config = Config::default();
        config.apply_override("http://192.168.1.20:8188");

        assert_eq!(config.active_target().url, "http://192.168.1.20:8188");
        assert_eq!(config.active_target().name, "192.168.1.20:8188");
        assert_eq!(config.switch_list_exclusion(), None);
        // One-off addresses stay out of the file.
        assert!(!config.serialize().contains("192.168.1.20"));
    }

    #[test]
    fn a_command_line_address_matching_a_configured_backend_selects_it() {
        let mut config = Config::default();
        config.apply_override("http://127.0.0.1:7860");

        assert_eq!(config.active_target().name, "A1111 WebUI");
        assert_eq!(config.active, 1);
        assert_eq!(config.switch_list_exclusion(), Some(1));
    }
}
