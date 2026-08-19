//! DiffusionFrame -- a lean desktop frame for ComfyUI, Stable Diffusion WebUI
//! and friends.
//!
//! It hosts the backend's existing web UI in the WebView2 runtime that already
//! ships with Windows, so there is no second browser engine resident in memory
//! and no GPU contention beyond what the toggle allows.

// No console window in release builds; the panic hook below takes over the job
// of reporting failures.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::error::Error;

use diffusionframe::{browser_args, cli, config, menu, net, platform, ui};

use tao::dpi::{PhysicalPosition, PhysicalSize};
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy, EventLoopWindowTarget};
#[cfg(windows)]
use tao::platform::windows::WindowExtWindows;
use tao::window::{Icon, Window, WindowBuilder};
#[cfg(windows)]
use wry::WebViewBuilderExtWindows;
use wry::{NewWindowResponse, WebContext, WebView, WebViewBuilder};

use config::Config;
use menu::MenuCommand;

/// Raw RGBA for the title-bar and taskbar icon, generated from the upstream
/// Material Symbols "iframe" glyph by `tools/make_icon.py`. Pre-rasterized so
/// the binary carries no image decoder.
const ICON_RGBA: &[u8] = include_bytes!("../assets/icon-32.rgba");
const ICON_SIZE: u32 = 32;

/// Messages the injected shortcut bridge, the offline page, the system menu
/// and the new-window handler send back to the event loop.
#[derive(Debug)]
enum UserEvent {
    /// Carries the window that sent it, since every window has this bridge.
    Ipc(usize, String),
    Menu(MenuCommand, isize),
    NewWindow(String),
    /// A page entered or left element fullscreen (a video's own fullscreen
    /// button, for instance), carrying the window it happened in.
    Fullscreen(usize, bool),
}

/// A window and the webview filling it. The main window and every window
/// opened from a link are the same shape, so lookups can treat them alike.
struct FrameWindow {
    /// Our own handle, so IPC from a window can be routed back to it. The
    /// main window is always 0.
    id: usize,
    window: Window,
    webview: WebView,
}

const MAIN_WINDOW: usize = 0;

impl FrameWindow {
    fn owns(&self, hwnd: isize) -> bool {
        hwnd_of(&self.window) == hwnd
    }

    /// Adopt the icon the page reported, replacing DiffusionFrame's own.
    fn set_icon(&self, pixels: Vec<u8>) {
        if let Ok(icon) = Icon::from_rgba(pixels, ui::ICON_SIZE, ui::ICON_SIZE) {
            self.window.set_window_icon(Some(icon));
        }
    }
}

#[cfg(windows)]
fn hwnd_of(window: &Window) -> isize {
    window.hwnd()
}

#[cfg(not(windows))]
fn hwnd_of(_window: &Window) -> isize {
    0
}

fn main() {
    install_panic_hook();

    let args = cli::parse(std::env::args().skip(1));

    if let Some(error) = &args.error {
        platform::show_error(&format!("{error}\n\n{}", cli::USAGE));
        std::process::exit(2);
    }
    if args.help {
        platform::show_info(cli::USAGE);
        return;
    }
    if args.version {
        platform::show_info(concat!("DiffusionFrame ", env!("CARGO_PKG_VERSION")));
        return;
    }

    // A restarting instance waits for its predecessor to release the WebView2
    // profile before touching it.
    if let Some(pid) = args.await_exit {
        platform::wait_for_process_exit(pid, 5_000);
    }

    if let Err(error) = run(args) {
        platform::show_error(&format!("DiffusionFrame could not start.\n\n{error}"));
        std::process::exit(1);
    }
}

fn run(args: cli::Args) -> Result<(), Box<dyn Error>> {
    let mut config = Config::load();
    if let Some(url) = &args.url {
        config.apply_override(url);
    }

    if config.low_priority {
        platform::set_low_priority();
    }

    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();

    // The system menu's commands arrive on the window procedure; forward them
    // into the event loop so they are handled where the state lives.
    let menu_proxy = proxy.clone();
    menu::set_handler(move |command, hwnd| {
        let _ = menu_proxy.send_event(UserEvent::Menu(command, hwnd));
    });

    let mut window_builder = WindowBuilder::new()
        .with_title(window_title(&config))
        .with_window_icon(app_icon())
        .with_inner_size(PhysicalSize::new(config.window.width, config.window.height))
        .with_min_inner_size(PhysicalSize::new(480, 360))
        .with_maximized(config.window.maximized);
    if let Some((x, y)) = config.window.position.filter(|&(x, y)| is_on_screen(x, y)) {
        window_builder = window_builder.with_position(PhysicalPosition::new(x, y));
    }
    let window = window_builder.build(&event_loop)?;

    let mut web_context = WebContext::new(Some(config::webview_data_dir()));

    let target = config.active_target().clone();
    let reachable = net::is_listening(&target.url);

    let mut builder = webview_builder(&mut web_context, &config, &proxy, MAIN_WINDOW)
        // Only the main window binds shortcuts: they act on the backend and
        // the config, neither of which a link window represents.
        .with_initialization_script_for_main_only(ui::shortcuts_script(), true)
        .with_devtools(cfg!(debug_assertions));

    builder = if reachable {
        builder.with_url(&target.url)
    } else {
        builder.with_html(ui::offline_page(
            &target,
            &config.targets,
            config.switch_list_exclusion(),
        ))
    };

    let main = FrameWindow {
        id: MAIN_WINDOW,
        webview: builder.build(&window)?,
        window,
    };

    if (config.zoom - 1.0).abs() > f64::EPSILON {
        let _ = main.webview.zoom(config.zoom);
    }
    decorate(&main.window, &config);
    watch_fullscreen(&main.webview, MAIN_WINDOW, &proxy);

    let mut children: Vec<FrameWindow> = Vec::new();
    let mut next_window_id = MAIN_WINDOW + 1;
    let mut minimized = false;

    event_loop.run(move |event, target, control_flow| {
        // Wait, not Poll: an idle frame should use no CPU at all.
        *control_flow = ControlFlow::Wait;

        match event {
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                window_id,
                ..
            } => {
                if main.window.id() == window_id {
                    capture_window_state(&main.window, &mut config);
                    config.save();
                    *control_flow = ControlFlow::Exit;
                } else {
                    // Dropping the entry tears down that window and its
                    // webview; the frame itself stays open.
                    children.retain(|child| child.window.id() != window_id);
                }
            }

            // Minimizing reports a zero-sized surface. Hiding the webview stops
            // compositing outright, and the memory hint lets WebView2 release
            // renderer caches -- both handed back on restore.
            Event::WindowEvent {
                event: WindowEvent::Resized(size),
                window_id,
                ..
            } if config.idle_throttle && main.window.id() == window_id => {
                let now_minimized = size.width == 0 || size.height == 0;
                if now_minimized != minimized {
                    minimized = now_minimized;
                    set_idle(&main.webview, minimized);
                }
            }

            Event::UserEvent(UserEvent::Ipc(id, message)) => {
                handle_message(&message, id, &main, &children, &mut config);
            }

            Event::UserEvent(UserEvent::Menu(command, hwnd)) => {
                handle_menu(command, hwnd, &main, &children, &mut config);
            }

            // WebView2 only expands an in-page fullscreen request (a video's
            // own fullscreen button, `Element.requestFullscreen()`) to fill
            // our window's bounds, not the monitor -- the same borderless
            // resize our own Ctrl+Shift+F uses gets it the rest of the way.
            Event::UserEvent(UserEvent::Fullscreen(id, entered)) => {
                if let Some(frame) = find(&main, &children, id) {
                    let fullscreen = entered.then_some(tao::window::Fullscreen::Borderless(None));
                    frame.window.set_fullscreen(fullscreen);
                }
            }

            Event::UserEvent(UserEvent::NewWindow(url)) => {
                let id = next_window_id;
                next_window_id += 1;
                match open_child(target, &mut web_context, &proxy, &config, &url, id) {
                    Ok(child) => children.push(child),
                    Err(error) => {
                        platform::show_error(&format!("Could not open {url}.\n\n{error}"))
                    }
                }
            }

            _ => {}
        }
    });
}

/// The webview settings every window shares, including the two restart-only
/// toggles.
fn webview_builder<'a>(
    web_context: &'a mut WebContext,
    config: &Config,
    proxy: &EventLoopProxy<UserEvent>,
    id: usize,
) -> WebViewBuilder<'a> {
    let new_window_proxy = proxy.clone();
    let ipc_proxy = proxy.clone();

    #[allow(unused_mut)]
    let mut builder = WebViewBuilder::new_with_web_context(web_context)
        .with_ipc_handler(move |request| {
            let _ = ipc_proxy.send_event(UserEvent::Ipc(id, request.into_body()));
        })
        // Every window adopts its page's favicon, so a link window is
        // recognisable in the taskbar rather than being a second copy of us.
        .with_initialization_script_for_main_only(ui::favicon_script(), true)
        // The window title always follows the page, so it says what is
        // actually on screen rather than lagging on the backend's name.
        .with_initialization_script_for_main_only(ui::title_script(), true)
        // Links that ask for a new page get their own frame window rather
        // than replacing what is on screen.
        .with_new_window_req_handler(move |url, _features| {
            let _ = new_window_proxy.send_event(UserEvent::NewWindow(url));
            NewWindowResponse::Deny
        })
        .with_hotkeys_zoom(true)
        // Matches the dark backgrounds these UIs use, so resizing and startup
        // do not flash white.
        .with_background_color((22, 24, 29, 255));

    // Only worth watching the page's colours when the title bar follows them.
    if config.titlebar.follows_page() {
        builder = builder.with_initialization_script_for_main_only(ui::background_script(), true);
    }

    // Both toggles live here: WebView2 reads its command line once, when the
    // browser environment is created.
    #[cfg(windows)]
    {
        builder = builder.with_additional_browser_args(browser_args::build(
            config.hardware_acceleration,
            config.colour_management,
        ));
    }

    builder
}

/// Open a link in its own frame window, sharing the parent's webview context
/// so the two windows use one browser process rather than two.
fn open_child(
    target: &EventLoopWindowTarget<UserEvent>,
    web_context: &mut WebContext,
    proxy: &EventLoopProxy<UserEvent>,
    config: &Config,
    url: &str,
    id: usize,
) -> Result<FrameWindow, Box<dyn Error>> {
    let window = WindowBuilder::new()
        .with_title(child_title(url))
        .with_window_icon(app_icon())
        .with_inner_size(PhysicalSize::new(1100, 800))
        .with_min_inner_size(PhysicalSize::new(480, 360))
        .build(target)?;

    let webview = webview_builder(web_context, config, proxy, id)
        .with_url(url)
        .build(&window)?;

    decorate(&window, config);
    watch_fullscreen(&webview, id, proxy);
    Ok(FrameWindow {
        id,
        window,
        webview,
    })
}

/// Make the page's own fullscreen requests (a video's fullscreen button)
/// resize the OS window, not just the content area.
#[cfg(windows)]
fn watch_fullscreen(webview: &WebView, id: usize, proxy: &EventLoopProxy<UserEvent>) {
    use webview2_com::ContainsFullScreenElementChangedEventHandler;
    use wry::WebViewExtWindows;

    let proxy = proxy.clone();
    let raw = webview.webview();
    let handler =
        ContainsFullScreenElementChangedEventHandler::create(Box::new(move |sender, _args| {
            let Some(sender) = sender else {
                return Ok(());
            };
            let mut contains = windows_core::BOOL(0);
            unsafe {
                sender.ContainsFullScreenElement(&mut contains)?;
            }
            let _ = proxy.send_event(UserEvent::Fullscreen(id, contains.as_bool()));
            Ok(())
        }));

    let mut token: i64 = 0;
    unsafe {
        let _ = raw.add_ContainsFullScreenElementChanged(&handler, &mut token);
    }
}

#[cfg(not(windows))]
fn watch_fullscreen(_webview: &WebView, _id: usize, _proxy: &EventLoopProxy<UserEvent>) {}

fn handle_message(
    message: &str,
    id: usize,
    main: &FrameWindow,
    children: &[FrameWindow],
    config: &mut Config,
) {
    // Reports about a page belong to the window that sent them; link windows
    // send these too.
    if let Some(payload) = message.strip_prefix("icon:") {
        if let Some(pixels) = ui::decode_icon(payload) {
            if let Some(frame) = find(main, children, id) {
                frame.set_icon(pixels);
            }
        }
        return;
    }
    if let Some(payload) = message.strip_prefix("background:") {
        if config.titlebar.follows_page() {
            if let (Some(colour), Some(frame)) =
                (ui::parse_background(payload), find(main, children, id))
            {
                platform::set_titlebar(hwnd_of(&frame.window), Some(colour));
            }
        }
        return;
    }
    if let Some(payload) = message.strip_prefix("title:") {
        if let (Some(page_title), Some(frame)) =
            (ui::parse_title(payload), find(main, children, id))
        {
            frame
                .window
                .set_title(&frame_title(&page_title, frame.id, config));
        }
        return;
    }

    // Everything below acts on the backend or the config, and only the main
    // window carries the shortcut bridge that sends it.
    if id != MAIN_WINDOW {
        return;
    }

    let window = &main.window;
    let webview = &main.webview;

    match message {
        // Sent on a timer by the offline page until the backend answers.
        "retry" => {
            let target = config.active_target().clone();
            if net::is_listening(&target.url) {
                let _ = webview.load_url(&target.url);
            }
        }

        "reload" => show_target(webview, config),

        "toggle-gpu" => {
            config.hardware_acceleration = !config.hardware_acceleration;
            restart(window, config);
        }

        "fullscreen" => {
            let fullscreen = window
                .fullscreen()
                .is_none()
                .then_some(tao::window::Fullscreen::Borderless(None));
            window.set_fullscreen(fullscreen);
        }

        "open-config" => {
            // Make sure there is a file to look at before opening the folder.
            config.save();
            platform::reveal_in_file_manager(&config::config_dir());
        }

        "devtools" => {
            #[cfg(debug_assertions)]
            webview.open_devtools();
        }

        "zoom-in" | "zoom-out" | "zoom-reset" => {
            config.zoom = match message {
                "zoom-in" => (config.zoom + 0.1).min(4.0),
                "zoom-out" => (config.zoom - 0.1).max(0.25),
                _ => 1.0,
            };
            let _ = webview.zoom(config.zoom);
            config.save();
        }

        _ => {
            if let Some(index) = message.strip_prefix("target:").and_then(|i| i.parse().ok()) {
                let switching = index < config.targets.len()
                    && (index != config.active || config.override_target.is_some());
                if switching {
                    // An explicit switch retires the command-line address.
                    config.override_target = None;
                    config.active = index;
                    window.set_title(&window_title(config));
                    show_target(webview, config);
                    // The freshly loaded page will announce its own title
                    // shortly; this is only the placeholder until it does.
                    config.save();
                }
            }
            // Anything else came from the hosted page, not from our bridge.
        }
    }
}

fn handle_menu(
    command: MenuCommand,
    hwnd: isize,
    main: &FrameWindow,
    children: &[FrameWindow],
    config: &mut Config,
) {
    match command {
        // Refresh acts on the window whose menu was used, not on the frame's
        // main page, so it works in link windows too.
        MenuCommand::Refresh => {
            let frame = std::iter::once(main)
                .chain(children)
                .find(|frame| frame.owns(hwnd))
                .unwrap_or(main);
            if std::ptr::eq(frame, main) {
                // Re-probe first, so refreshing a stopped backend lands on the
                // offline page instead of a Chromium error.
                show_target(&frame.webview, config);
            } else {
                let _ = frame.webview.reload();
            }
        }

        // Cache is shared by every window through the common WebContext, so
        // clearing from the main webview is enough regardless of which
        // window's menu was used. The restart isn't strictly needed to see
        // the effect, but a stale service worker or WASM module can
        // otherwise stay resident in the renderer that served it -- catching
        // both is the point of this specific button.
        MenuCommand::ClearCacheAndRestart => {
            clear_cache(&main.webview);
            restart(&main.window, config);
        }

        // Both toggles are command-line options to the WebView2 environment,
        // which is fixed once created -- so they can only apply on a restart.
        MenuCommand::ColourManagement => {
            config.colour_management = !config.colour_management;
            restart(&main.window, config);
        }
        MenuCommand::HardwareAcceleration => {
            config.hardware_acceleration = !config.hardware_acceleration;
            restart(&main.window, config);
        }
    }
}

/// Persist everything worth keeping, then relaunch so the new browser
/// arguments take hold.
fn restart(window: &Window, config: &mut Config) -> ! {
    capture_window_state(window, config);
    config.save();
    let carried = config.override_target.as_ref().map(|t| t.url.clone());
    platform::restart(carried.as_deref())
}

/// Navigate to the active backend, falling back to the offline placeholder if
/// nothing is listening.
fn show_target(webview: &WebView, config: &Config) {
    let target = config.active_target();
    let _ = if net::is_listening(&target.url) {
        webview.load_url(&target.url)
    } else {
        webview.load_html(&ui::offline_page(
            target,
            &config.targets,
            config.switch_list_exclusion(),
        ))
    };
}

/// Clear only cache-shaped data: the disk cache, the Cache Storage API, and
/// stale service workers.
///
/// Deliberately narrower than [`WebView::clear_all_browsing_data`], which
/// this button used before: "all browsing data" also means cookies, local
/// storage and IndexedDB, and ComfyUI keeps its open tabs and workflow state
/// in exactly those -- wiping them along with the cache silently threw that
/// state away too. A menu item labelled "clear cache" should not sign you
/// out and close your tabs as a side effect.
#[cfg(windows)]
fn clear_cache(webview: &WebView) {
    use webview2_com::ClearBrowsingDataCompletedHandler;
    use webview2_com::Microsoft::Web::WebView2::Win32::{
        ICoreWebView2Profile2, ICoreWebView2_13, COREWEBVIEW2_BROWSING_DATA_KINDS_CACHE_STORAGE,
        COREWEBVIEW2_BROWSING_DATA_KINDS_DISK_CACHE,
        COREWEBVIEW2_BROWSING_DATA_KINDS_SERVICE_WORKERS,
    };
    use windows_core::Interface;
    use wry::WebViewExtWindows;

    let kinds = COREWEBVIEW2_BROWSING_DATA_KINDS_DISK_CACHE
        | COREWEBVIEW2_BROWSING_DATA_KINDS_CACHE_STORAGE
        | COREWEBVIEW2_BROWSING_DATA_KINDS_SERVICE_WORKERS;

    let profile = unsafe {
        webview
            .webview()
            .cast::<ICoreWebView2_13>()
            .and_then(|webview| webview.Profile())
            .and_then(|profile| profile.cast::<ICoreWebView2Profile2>())
    };

    if let Ok(profile) = profile {
        unsafe {
            let _ = profile.ClearBrowsingData(
                kinds,
                &ClearBrowsingDataCompletedHandler::create(Box::new(move |_| Ok(()))),
            );
        }
    }
}

#[cfg(not(windows))]
fn clear_cache(_webview: &WebView) {}

fn set_idle(webview: &WebView, idle: bool) {
    let _ = webview.set_visible(!idle);

    #[cfg(windows)]
    {
        use wry::{MemoryUsageLevel, WebViewExtWindows};
        let level = if idle {
            MemoryUsageLevel::Low
        } else {
            MemoryUsageLevel::Normal
        };
        let _ = webview.set_memory_usage_level(level);
    }
}

fn find<'a>(
    main: &'a FrameWindow,
    children: &'a [FrameWindow],
    id: usize,
) -> Option<&'a FrameWindow> {
    std::iter::once(main)
        .chain(children)
        .find(|frame| frame.id == id)
}

/// Give a freshly built window its system menu and title bar colour.
fn decorate(window: &Window, config: &Config) {
    let hwnd = hwnd_of(window);
    menu::install(hwnd, config.colour_management, config.hardware_acceleration);
    // Page-following windows start here too and are repainted once the page
    // reports, so the caption never flashes the system colour first.
    platform::set_titlebar(hwnd, config.titlebar.initial_colour());
}

fn app_icon() -> Option<Icon> {
    Icon::from_rgba(ICON_RGBA.to_vec(), ICON_SIZE, ICON_SIZE).ok()
}

fn capture_window_state(window: &Window, config: &mut Config) {
    config.window.maximized = window.is_maximized();

    // Size and position while maximized or fullscreen describe the screen, not
    // the window the user actually arranged, so keep the stored values.
    if config.window.maximized || window.fullscreen().is_some() {
        return;
    }

    let size = window.inner_size();
    if size.width > 0 && size.height > 0 {
        config.window.width = size.width;
        config.window.height = size.height;
    }
    if let Ok(position) = window.outer_position() {
        config.window.position = Some((position.x, position.y));
    }
}

/// Used only until the page announces its own title -- at startup, and
/// briefly after switching backends.
fn window_title(config: &Config) -> String {
    format!(
        "{} — DiffusionFrame{}",
        config.active_target().name,
        status_suffix(MAIN_WINDOW, config)
    )
}

/// Used only until a link window's page announces its own title.
fn child_title(url: &str) -> String {
    let placeholder = match net::host_and_port(url) {
        Some((host, port)) => format!("{host}:{port}"),
        None => url.to_string(),
    };
    format!("{placeholder} — DiffusionFrame")
}

/// The window caption once the page has reported its title: the title itself,
/// suffixed with DiffusionFrame and, on the main window, the two restart-only
/// toggles' status.
fn frame_title(page_title: &str, id: usize, config: &Config) -> String {
    format!("{page_title} — DiffusionFrame{}", status_suffix(id, config))
}

/// Only the main window carries GPU/colour status -- a link window shows an
/// arbitrary third-party page and shares the same settings, so repeating the
/// indicator there would just be clutter.
fn status_suffix(id: usize, config: &Config) -> String {
    if id != MAIN_WINDOW {
        return String::new();
    }
    let mut suffix = String::new();
    if !config.hardware_acceleration {
        suffix.push_str("  ·  GPU off");
    }
    if !config.colour_management {
        suffix.push_str("  ·  unmanaged colour");
    }
    suffix
}

/// Guard against restoring a window onto a monitor that no longer exists.
fn is_on_screen(x: i32, y: i32) -> bool {
    (-32_000..32_000).contains(&x) && (-32_000..32_000).contains(&y)
}

fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // Release builds have no console, so a bare panic would be a silent
        // disappearance.
        platform::show_error(&format!("DiffusionFrame stopped unexpectedly.\n\n{info}"));
        previous(info);
    }));
}
