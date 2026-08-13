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

use diffusionframe::{browser_args, cli, config, net, platform, ui};

use tao::dpi::{PhysicalPosition, PhysicalSize};
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::window::WindowBuilder;
#[cfg(windows)]
use wry::WebViewBuilderExtWindows;
use wry::{NewWindowResponse, WebContext, WebViewBuilder};

use config::Config;

/// Messages the injected shortcut bridge and the offline page send back to the
/// event loop.
#[derive(Debug)]
enum UserEvent {
    Ipc(String),
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

    let mut window_builder = WindowBuilder::new()
        .with_title(window_title(&config))
        .with_inner_size(PhysicalSize::new(config.window.width, config.window.height))
        .with_min_inner_size(PhysicalSize::new(480, 360))
        .with_maximized(config.window.maximized);
    if let Some((x, y)) = config.window.position.filter(|&(x, y)| is_on_screen(x, y)) {
        window_builder = window_builder.with_position(PhysicalPosition::new(x, y));
    }
    let window = window_builder.build(&event_loop)?;

    let proxy = event_loop.create_proxy();
    let mut web_context = WebContext::new(Some(config::webview_data_dir()));

    let target = config.active_target().clone();
    let reachable = net::is_listening(&target.url);

    let mut builder = WebViewBuilder::new_with_web_context(&mut web_context)
        .with_initialization_script_for_main_only(ui::shortcuts_script(), true)
        .with_ipc_handler(move |request| {
            let _ = proxy.send_event(UserEvent::Ipc(request.into_body()));
        })
        // Links out of the backend UI (docs, model pages) belong in the user's
        // real browser, not in a second webview this process has to host.
        .with_new_window_req_handler(|url, _features| {
            if url.starts_with("http://") || url.starts_with("https://") {
                platform::open_external(&url);
            }
            NewWindowResponse::Deny
        })
        .with_hotkeys_zoom(true)
        .with_devtools(cfg!(debug_assertions))
        // Matches the dark backgrounds these UIs use, so resizing and startup
        // do not flash white.
        .with_background_color((22, 24, 29, 255));

    // The acceleration toggle lives here: WebView2 reads its command line once,
    // when the environment is created.
    #[cfg(windows)]
    {
        builder =
            builder.with_additional_browser_args(browser_args::build(config.hardware_acceleration));
    }

    builder = if reachable {
        builder.with_url(&target.url)
    } else {
        builder.with_html(ui::offline_page(
            &target,
            &config.targets,
            config.switch_list_exclusion(),
        ))
    };

    let webview = builder.build(&window)?;

    if (config.zoom - 1.0).abs() > f64::EPSILON {
        let _ = webview.zoom(config.zoom);
    }

    let mut minimized = false;

    event_loop.run(move |event, _, control_flow| {
        // Wait, not Poll: an idle frame should use no CPU at all.
        *control_flow = ControlFlow::Wait;

        match event {
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => {
                capture_window_state(&window, &mut config);
                config.save();
                *control_flow = ControlFlow::Exit;
            }

            // Minimizing reports a zero-sized surface. Hiding the webview stops
            // compositing outright, and the memory hint lets WebView2 release
            // renderer caches -- both handed back on restore.
            Event::WindowEvent {
                event: WindowEvent::Resized(size),
                ..
            } if config.idle_throttle => {
                let now_minimized = size.width == 0 || size.height == 0;
                if now_minimized != minimized {
                    minimized = now_minimized;
                    set_idle(&webview, minimized);
                }
            }

            Event::UserEvent(UserEvent::Ipc(message)) => {
                handle_message(&message, &window, &webview, &mut config);
            }

            _ => {}
        }
    });
}

fn handle_message(
    message: &str,
    window: &tao::window::Window,
    webview: &wry::WebView,
    config: &mut Config,
) {
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
            // WebView2 fixes its command line when the environment is created,
            // so this can only take effect on a fresh process.
            config.hardware_acceleration = !config.hardware_acceleration;
            capture_window_state(window, config);
            config.save();
            let carried = config.override_target.as_ref().map(|t| t.url.clone());
            platform::restart(carried.as_deref());
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

/// Navigate to the active backend, falling back to the offline placeholder if
/// nothing is listening.
fn show_target(webview: &wry::WebView, config: &Config) {
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

fn set_idle(webview: &wry::WebView, idle: bool) {
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

fn capture_window_state(window: &tao::window::Window, config: &mut Config) {
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
    let suffix = if config.hardware_acceleration {
        ""
    } else {
        "  ·  GPU off"
    };
    format!("{} — DiffusionFrame{suffix}", config.active_target().name)
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
