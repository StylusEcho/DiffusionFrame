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
    Ipc(String),
    Menu(MenuCommand, isize),
    NewWindow(String),
}

/// A window and the webview filling it. The main window and every window
/// opened from a link are the same shape, so lookups can treat them alike.
struct FrameWindow {
    window: Window,
    webview: WebView,
}

impl FrameWindow {
    #[cfg(windows)]
    fn owns(&self, hwnd: isize) -> bool {
        self.window.hwnd() == hwnd
    }

    #[cfg(not(windows))]
    fn owns(&self, _hwnd: isize) -> bool {
        false
    }
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

    let ipc_proxy = proxy.clone();
    let mut builder = webview_builder(&mut web_context, &config, &proxy)
        .with_initialization_script_for_main_only(ui::shortcuts_script(), true)
        .with_ipc_handler(move |request| {
            let _ = ipc_proxy.send_event(UserEvent::Ipc(request.into_body()));
        })
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
        webview: builder.build(&window)?,
        window,
    };

    if (config.zoom - 1.0).abs() > f64::EPSILON {
        let _ = main.webview.zoom(config.zoom);
    }
    install_menu(&main.window, &config);

    let mut children: Vec<FrameWindow> = Vec::new();
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

            Event::UserEvent(UserEvent::Ipc(message)) => {
                handle_message(&message, &main, &mut config);
            }

            Event::UserEvent(UserEvent::Menu(command, hwnd)) => {
                handle_menu(command, hwnd, &main, &children, &mut config);
            }

            Event::UserEvent(UserEvent::NewWindow(url)) => {
                match open_child(target, &mut web_context, &proxy, &config, &url) {
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
) -> WebViewBuilder<'a> {
    let new_window_proxy = proxy.clone();

    #[allow(unused_mut)]
    let mut builder = WebViewBuilder::new_with_web_context(web_context)
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

    // Both toggles live here: WebView2 reads its command line once, when the
    // browser environment is created.
    #[cfg(windows)]
    {
        builder = builder.with_additional_browser_args(browser_args::build(
            config.hardware_acceleration,
            config.colour_management,
        ));
    }
    #[cfg(not(windows))]
    let _ = config;

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
) -> Result<FrameWindow, Box<dyn Error>> {
    let window = WindowBuilder::new()
        .with_title(child_title(url))
        .with_window_icon(app_icon())
        .with_inner_size(PhysicalSize::new(1100, 800))
        .with_min_inner_size(PhysicalSize::new(480, 360))
        .build(target)?;

    let webview = webview_builder(web_context, config, proxy)
        .with_url(url)
        .build(&window)?;

    install_menu(&window, config);
    Ok(FrameWindow { window, webview })
}

fn handle_message(message: &str, main: &FrameWindow, config: &mut Config) {
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

#[cfg(windows)]
fn install_menu(window: &Window, config: &Config) {
    menu::install(
        window.hwnd(),
        config.colour_management,
        config.hardware_acceleration,
    );
}

#[cfg(not(windows))]
fn install_menu(_window: &Window, _config: &Config) {}

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

fn window_title(config: &Config) -> String {
    let mut title = format!("{} — DiffusionFrame", config.active_target().name);
    if !config.hardware_acceleration {
        title.push_str("  ·  GPU off");
    }
    if !config.colour_management {
        title.push_str("  ·  unmanaged colour");
    }
    title
}

/// Link windows are titled by where they went, since they show arbitrary pages.
fn child_title(url: &str) -> String {
    match net::host_and_port(url) {
        Some((host, port)) => format!("{host}:{port} — DiffusionFrame"),
        None => format!("{url} — DiffusionFrame"),
    }
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
