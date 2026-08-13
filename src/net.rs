//! A single TCP connect used to tell "backend not started yet" apart from
//! "backend is broken", so the frame can show something better than a
//! Chromium error page. Hand-rolled to avoid pulling in a URL parser.

use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

const PROBE_TIMEOUT: Duration = Duration::from_millis(600);

/// Split `http://host:port/path` into its host and port.
pub fn host_and_port(url: &str) -> Option<(String, u16)> {
    let (scheme, rest) = url.split_once("://")?;
    let default_port = match scheme.to_ascii_lowercase().as_str() {
        "http" => 80,
        "https" => 443,
        _ => return None,
    };

    // Strip path, query and fragment, then any userinfo.
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let authority = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    if authority.is_empty() {
        return None;
    }

    // IPv6 literals are bracketed, so the port is whatever follows the
    // closing bracket rather than the last colon.
    if let Some(end) = authority
        .strip_prefix('[')
        .and_then(|_| authority.find(']'))
    {
        let host = &authority[1..end];
        let port = match authority[end + 1..].strip_prefix(':') {
            Some(port) => port.parse().ok()?,
            None => default_port,
        };
        return Some((host.to_string(), port));
    }

    match authority.split_once(':') {
        Some((host, port)) => Some((host.to_string(), port.parse().ok()?)),
        None => Some((authority.to_string(), default_port)),
    }
}

/// True when something is listening. Deliberately a connect-and-drop rather
/// than an HTTP request: it costs nothing and never touches backend state.
pub fn is_listening(url: &str) -> bool {
    let Some((host, port)) = host_and_port(url) else {
        // Not a URL shape we can probe, so let the webview try it directly.
        return true;
    };

    let Ok(addrs) = (host.as_str(), port).to_socket_addrs() else {
        return false;
    };

    addrs
        .into_iter()
        .any(|addr| TcpStream::connect_timeout(&addr, PROBE_TIMEOUT).is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_explicit_ports() {
        assert_eq!(
            host_and_port("http://127.0.0.1:8188"),
            Some(("127.0.0.1".into(), 8188))
        );
        assert_eq!(
            host_and_port("http://127.0.0.1:8188/some/path?x=1#frag"),
            Some(("127.0.0.1".into(), 8188))
        );
    }

    #[test]
    fn falls_back_to_scheme_default_ports() {
        assert_eq!(
            host_and_port("http://localhost"),
            Some(("localhost".into(), 80))
        );
        assert_eq!(
            host_and_port("https://example.test/x"),
            Some(("example.test".into(), 443))
        );
    }

    #[test]
    fn handles_ipv6_literals() {
        assert_eq!(
            host_and_port("http://[::1]:8188/"),
            Some(("::1".into(), 8188))
        );
        assert_eq!(host_and_port("http://[::1]/"), Some(("::1".into(), 80)));
    }

    #[test]
    fn strips_userinfo() {
        assert_eq!(
            host_and_port("http://user:pass@127.0.0.1:7860"),
            Some(("127.0.0.1".into(), 7860))
        );
    }

    #[test]
    fn rejects_unprobeable_urls() {
        assert_eq!(host_and_port("file:///C:/x.html"), None);
        assert_eq!(host_and_port("not a url"), None);
        assert_eq!(host_and_port("http://"), None);
        assert_eq!(host_and_port("http://127.0.0.1:notaport"), None);
    }

    #[test]
    fn unprobeable_urls_are_left_to_the_webview() {
        assert!(is_listening("file:///C:/x.html"));
    }
}
