//! Command-line parsing.
//!
//! Hand-rolled for the same reason the config is: a handful of options does
//! not justify an argument-parsing dependency.

use crate::config::Target;

pub const USAGE: &str = "\
DiffusionFrame — a lean desktop frame for ComfyUI, Stable Diffusion WebUI and friends

USAGE:
    diffusionframe [ADDRESS]
    diffusionframe --url <ADDRESS>

ADDRESS accepts any of:
    8188                     port on 127.0.0.1
    :8188                    port on 127.0.0.1
    192.168.1.20:8188        host and port
    http://127.0.0.1:8188    full URL (https and [::1] also work)

    Given without a port, http defaults to 80. The address overrides the
    configured backend for this run only; it is not written to the config.

OPTIONS:
    -u, --url <ADDRESS>   Backend to open, same forms as above
    -h, --help            Show this message
    -V, --version         Show the version";

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Args {
    /// Backend address given on the command line, overriding the config.
    pub url: Option<String>,
    /// PID to wait on before starting; set only by an in-place restart.
    pub await_exit: Option<u32>,
    pub help: bool,
    pub version: bool,
    /// Malformed input, reported instead of being silently ignored.
    pub error: Option<String>,
}

pub fn parse<I: IntoIterator<Item = String>>(args: I) -> Args {
    let mut parsed = Args::default();
    let mut args = args.into_iter();

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => parsed.help = true,
            "-V" | "--version" => parsed.version = true,
            "-u" | "--url" => match args.next() {
                Some(value) => set_url(&mut parsed, &value),
                None => parsed.error = Some(format!("{arg} needs an address.")),
            },
            "--await-exit" => parsed.await_exit = args.next().and_then(|pid| pid.parse().ok()),
            _ if arg.starts_with('-') => {
                parsed.error = Some(format!("Unknown option: {arg}"));
            }
            _ if parsed.url.is_none() => set_url(&mut parsed, &arg),
            _ => parsed.error = Some(format!("Unexpected argument: {arg}")),
        }
    }

    parsed
}

fn set_url(parsed: &mut Args, value: &str) {
    match normalize(value) {
        Some(url) => parsed.url = Some(url),
        None => parsed.error = Some(format!("Not a usable address: {value}")),
    }
}

/// Expand the shorthand forms into a full URL.
pub fn normalize(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }

    let candidate = if value.contains("://") {
        value.to_string()
    } else if let Some(port) = value.strip_prefix(':') {
        format!("http://127.0.0.1:{port}")
    } else if value.chars().all(|c| c.is_ascii_digit()) {
        format!("http://127.0.0.1:{value}")
    } else {
        format!("http://{value}")
    };

    // Reuse the probe's parser as the validator, so anything accepted here is
    // something the rest of the app can actually reach.
    crate::net::host_and_port(&candidate)?;
    Some(candidate)
}

/// Turn a command-line address into the backend to show.
///
/// A URL that matches something already configured selects that entry, keeping
/// its name and letting the choice persist; anything else becomes a one-off
/// target that is never written to the config.
pub fn resolve(url: &str, targets: &[Target]) -> Result<usize, Target> {
    let matches = |a: &str, b: &str| a.trim_end_matches('/') == b.trim_end_matches('/');

    match targets.iter().position(|target| matches(&target.url, url)) {
        Some(index) => Ok(index),
        None => Err(Target {
            name: display_name(url),
            url: url.to_string(),
        }),
    }
}

/// Label the window with `host:port` rather than the whole URL.
fn display_name(url: &str) -> String {
    match crate::net::host_and_port(url) {
        Some((host, port)) => format!("{host}:{port}"),
        None => url.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_str(args: &[&str]) -> Args {
        parse(args.iter().map(|arg| arg.to_string()))
    }

    #[test]
    fn expands_shorthand_addresses() {
        assert_eq!(normalize("8188").as_deref(), Some("http://127.0.0.1:8188"));
        assert_eq!(normalize(":7860").as_deref(), Some("http://127.0.0.1:7860"));
        assert_eq!(
            normalize("192.168.1.20:8188").as_deref(),
            Some("http://192.168.1.20:8188")
        );
        assert_eq!(normalize("localhost").as_deref(), Some("http://localhost"));
    }

    #[test]
    fn passes_full_urls_through() {
        assert_eq!(
            normalize("https://gpu.local:8188/").as_deref(),
            Some("https://gpu.local:8188/")
        );
        assert_eq!(
            normalize("http://[::1]:8188").as_deref(),
            Some("http://[::1]:8188")
        );
    }

    #[test]
    fn rejects_addresses_the_app_could_not_reach() {
        assert_eq!(normalize(""), None);
        assert_eq!(normalize("  "), None);
        assert_eq!(normalize("ftp://example.test"), None);
        assert_eq!(normalize("127.0.0.1:notaport"), None);
    }

    #[test]
    fn accepts_positional_and_flag_forms_alike() {
        let expected = Some("http://127.0.0.1:8188".to_string());
        assert_eq!(parse_str(&["8188"]).url, expected);
        assert_eq!(parse_str(&["--url", "8188"]).url, expected);
        assert_eq!(parse_str(&["-u", "127.0.0.1:8188"]).url, expected);
    }

    #[test]
    fn reports_bad_input_rather_than_ignoring_it() {
        assert!(parse_str(&["--nope"]).error.is_some());
        assert!(parse_str(&["--url"]).error.is_some());
        assert!(parse_str(&["8188", "7860"]).error.is_some());
        assert!(parse_str(&["ftp://example.test"]).error.is_some());
    }

    #[test]
    fn recognises_the_restart_handshake() {
        let args = parse_str(&["--await-exit", "4321"]);
        assert_eq!(args.await_exit, Some(4321));
        assert_eq!(args.url, None);
    }

    #[test]
    fn flags_are_recognised() {
        assert!(parse_str(&["--help"]).help);
        assert!(parse_str(&["-h"]).help);
        assert!(parse_str(&["-V"]).version);
    }

    #[test]
    fn a_configured_backend_is_selected_rather_than_duplicated() {
        let targets = vec![
            Target {
                name: "ComfyUI".into(),
                url: "http://127.0.0.1:8188".into(),
            },
            Target {
                name: "Forge".into(),
                url: "http://127.0.0.1:7861".into(),
            },
        ];
        assert_eq!(resolve("http://127.0.0.1:7861", &targets), Ok(1));
        // A trailing slash is the same backend.
        assert_eq!(resolve("http://127.0.0.1:8188/", &targets), Ok(0));

        let one_off = resolve("http://192.168.1.20:8188", &targets).unwrap_err();
        assert_eq!(one_off.name, "192.168.1.20:8188");
        assert_eq!(one_off.url, "http://192.168.1.20:8188");
    }
}
