//! The front-end DiffusionFrame owns: the small scripts injected into the
//! backend's own UI, and the offline placeholder shown when the backend is not
//! listening yet.

use crate::config::Target;

/// Side of the icon DiffusionFrame asks the page for, in pixels.
pub const ICON_SIZE: u32 = 32;
const ICON_BYTES: usize = (ICON_SIZE * ICON_SIZE * 4) as usize;

/// Keyboard shortcuts, forwarded to the event loop over wry's IPC channel.
///
/// The webview swallows key events before the window ever sees them, so this
/// is the only reliable way to bind chords. Everything is behind Ctrl+Shift to
/// stay clear of the backends' own bindings (ComfyUI uses plain Ctrl+S,
/// Ctrl+O, Ctrl+Z and friends), and unmatched keys fall through untouched.
pub fn shortcuts_script() -> String {
    // Bound in the capture phase so the page cannot swallow these first.
    r#"
(function () {
  if (window.__diffusionframe) return;
  window.__diffusionframe = true;

  var send = function (message) {
    try { window.ipc.postMessage(message); } catch (e) {}
  };

  window.addEventListener('keydown', function (event) {
    if (!event.ctrlKey || !event.shiftKey || event.altKey) return;

    var message = null;
    var code = event.code;

    if (code.indexOf('Digit') === 0) {
      var index = parseInt(code.slice(5), 10);
      if (index >= 1 && index <= 9) message = 'target:' + (index - 1);
    } else if (code === 'KeyG') {
      message = 'toggle-gpu';
    } else if (code === 'KeyR') {
      message = 'reload';
    } else if (code === 'KeyF') {
      message = 'fullscreen';
    } else if (code === 'KeyO') {
      message = 'open-config';
    } else if (code === 'KeyD') {
      message = 'devtools';
    } else if (code === 'Equal' || code === 'NumpadAdd') {
      message = 'zoom-in';
    } else if (code === 'Minus' || code === 'NumpadSubtract') {
      message = 'zoom-out';
    } else if (code === 'Digit0' || code === 'Numpad0') {
      message = 'zoom-reset';
    }

    if (message) {
      event.preventDefault();
      event.stopPropagation();
      send(message);
    }
  }, true);
})();
"#
    .to_string()
}

/// Reports the page's favicon as raw pixels.
///
/// The canvas does the decoding, so DiffusionFrame handles whatever format the
/// site uses -- .ico, PNG, SVG -- without linking an image decoder. What comes
/// back over IPC is already the RGBA buffer the window icon wants.
pub fn favicon_script() -> String {
    format!(
        r#"
(function () {{
  if (window.__diffusionframeIcon) return;
  window.__diffusionframeIcon = true;

  var SIZE = {size};

  var candidate = function () {{
    var links = document.querySelectorAll('link[rel~="icon" i]');
    var best = null;
    var bestSize = -1;
    for (var i = 0; i < links.length; i++) {{
      var sizes = (links[i].getAttribute('sizes') || '').toLowerCase();
      var size = 0;
      if (sizes === 'any') {{
        // Usually an SVG, which scales to whatever we ask for.
        size = 4096;
      }} else {{
        var match = sizes.match(/(\d+)x(\d+)/);
        if (match) size = parseInt(match[1], 10);
      }}
      if (size > bestSize) {{ bestSize = size; best = links[i].href; }}
    }}
    return best || (location.origin + '/favicon.ico');
  }};

  var report = function () {{
    var url = candidate();
    if (!url) return;
    var image = new Image();
    // Deliberately no crossOrigin: a same-origin favicon then loads without
    // needing CORS headers, and a cross-origin one taints the canvas and is
    // dropped by the catch below.
    image.onload = function () {{
      try {{
        var canvas = document.createElement('canvas');
        canvas.width = SIZE;
        canvas.height = SIZE;
        var context = canvas.getContext('2d');
        context.clearRect(0, 0, SIZE, SIZE);
        context.drawImage(image, 0, 0, SIZE, SIZE);
        var pixels = context.getImageData(0, 0, SIZE, SIZE).data;
        var binary = '';
        for (var i = 0; i < pixels.length; i++) binary += String.fromCharCode(pixels[i]);
        try {{ window.ipc.postMessage('icon:' + btoa(binary)); }} catch (e) {{}}
      }} catch (e) {{}}
    }};
    image.src = url;
  }};

  if (document.readyState === 'loading') {{
    document.addEventListener('DOMContentLoaded', report);
  }} else {{
    report();
  }}
}})();
"#,
        size = ICON_SIZE
    )
}

/// Reports the page's background colour so the title bar can match it.
///
/// Only injected when the title bar is set to follow the page. The observer
/// catches in-page theme switches, which would otherwise leave the caption
/// showing the old colour until a reload.
pub fn background_script() -> String {
    r#"
(function () {
  if (window.__diffusionframeBackground) return;
  window.__diffusionframeBackground = true;

  var opaque = function (element) {
    if (!element) return null;
    var colour = getComputedStyle(element).backgroundColor;
    var match = colour && colour.match(
      /rgba?\(\s*(\d+)\s*,\s*(\d+)\s*,\s*(\d+)(?:\s*,\s*([\d.]+))?/
    );
    if (!match) return null;
    // A see-through body tells us nothing; fall through to the root element.
    if (match[4] !== undefined && parseFloat(match[4]) < 0.5) return null;
    return match[1] + ',' + match[2] + ',' + match[3];
  };

  var last = null;
  var report = function () {
    var colour = opaque(document.body) || opaque(document.documentElement);
    if (colour && colour !== last) {
      last = colour;
      try { window.ipc.postMessage('background:' + colour); } catch (e) {}
    }
  };

  var pending = null;
  var schedule = function () {
    if (pending) clearTimeout(pending);
    pending = setTimeout(report, 250);
  };

  var watch = function () {
    report();
    // Theme switches show up as class or style changes on these two elements.
    var observer = new MutationObserver(schedule);
    var options = { attributes: true, attributeFilter: ['class', 'style', 'data-theme'] };
    observer.observe(document.documentElement, options);
    if (document.body) observer.observe(document.body, options);
  };

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', watch);
  } else {
    watch();
  }
})();
"#
    .to_string()
}

/// Decode the RGBA buffer sent by [`favicon_script`].
///
/// Returns `None` for anything that is not exactly one icon's worth of pixels,
/// so a truncated or malformed message is dropped rather than shown.
pub fn decode_icon(payload: &str) -> Option<Vec<u8>> {
    let pixels = decode_base64(payload)?;
    (pixels.len() == ICON_BYTES).then_some(pixels)
}

/// Parse the `r,g,b` sent by [`background_script`].
pub fn parse_background(payload: &str) -> Option<(u8, u8, u8)> {
    let channels: Vec<&str> = payload.split(',').collect();
    let [red, green, blue] = channels[..] else {
        return None;
    };
    Some((
        red.trim().parse().ok()?,
        green.trim().parse().ok()?,
        blue.trim().parse().ok()?,
    ))
}

/// Standard base64, no padding tolerance beyond trailing `=`.
///
/// Hand-rolled to keep the dependency list at three crates; the only producer
/// is `btoa` in the script above.
fn decode_base64(input: &str) -> Option<Vec<u8>> {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    let mut out = Vec::with_capacity(input.len() / 4 * 3);
    let mut accumulator: u32 = 0;
    let mut bits = 0;

    for byte in input.bytes() {
        if byte == b'=' {
            break;
        }
        if byte.is_ascii_whitespace() {
            continue;
        }
        let value = ALPHABET.iter().position(|&c| c == byte)? as u32;
        accumulator = (accumulator << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((accumulator >> bits) as u8);
        }
    }

    // Leftover bits must be zero padding, never a dropped byte.
    ((accumulator & ((1 << bits) - 1)) == 0).then_some(out)
}

/// Shown when nothing is listening on the target's port. Self-contained, and
/// it retries on a timer so starting the backend afterwards just works.
///
/// `active` is the configured backend on screen, or `None` when the address
/// came from the command line and is not one of the configured entries.
pub fn offline_page(target: &Target, targets: &[Target], active: Option<usize>) -> String {
    let mut alternatives = String::new();
    for (index, candidate) in targets.iter().enumerate().take(9) {
        if Some(index) == active {
            continue;
        }
        alternatives.push_str(&format!(
            "<li><kbd>Ctrl</kbd><kbd>Shift</kbd><kbd>{}</kbd><span>{}</span></li>",
            index + 1,
            escape(&candidate.name)
        ));
    }

    format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{name} is not responding</title>
<style>
  :root {{ color-scheme: dark; }}
  * {{ box-sizing: border-box; }}
  body {{
    margin: 0; min-height: 100vh; display: grid; place-items: center;
    background: #16181d; color: #d8dbe2; padding: 2rem;
    font: 15px/1.6 "Segoe UI Variable Text", "Segoe UI", system-ui, sans-serif;
  }}
  main {{ max-width: 32rem; width: 100%; }}
  h1 {{ font-size: 1.35rem; font-weight: 600; margin: 0 0 .5rem; color: #f2f4f8; }}
  p {{ margin: 0 0 1rem; color: #9aa1ae; }}
  code {{
    font-family: "Cascadia Mono", Consolas, ui-monospace, monospace;
    background: #202329; border: 1px solid #2c3038; border-radius: 5px;
    padding: .1rem .4rem; color: #c8cdd6; font-size: .9em;
  }}
  .status {{
    display: flex; align-items: center; gap: .6rem;
    border-top: 1px solid #24272e; margin-top: 1.5rem; padding-top: 1.25rem;
    color: #7d8492; font-size: .9rem;
  }}
  .pulse {{
    width: 8px; height: 8px; border-radius: 50%; background: #d99a3f;
    animation: pulse 1.6s ease-in-out infinite; flex: none;
  }}
  @keyframes pulse {{ 0%, 100% {{ opacity: .35; }} 50% {{ opacity: 1; }} }}
  @media (prefers-reduced-motion: reduce) {{ .pulse {{ animation: none; }} }}
  ul {{ list-style: none; margin: 1rem 0 0; padding: 0; }}
  li {{ display: flex; align-items: center; gap: .35rem; margin-bottom: .4rem; }}
  li span {{ margin-left: .5rem; color: #9aa1ae; }}
  kbd {{
    font-family: inherit; font-size: .78rem; background: #202329;
    border: 1px solid #333741; border-bottom-width: 2px; border-radius: 4px;
    padding: .1rem .35rem; color: #b6bcc7;
  }}
</style>
</head>
<body>
<main>
  <h1>Waiting for {name}</h1>
  <p>Nothing is listening on <code>{url}</code> yet. Start the backend and this
     page will connect on its own &mdash; no need to reload.</p>
  {alternatives}
  <div class="status"><span class="pulse"></span><span id="status">Retrying every 2 seconds</span></div>
</main>
<script>
  var attempts = 0;
  setInterval(function () {{
    attempts++;
    document.getElementById('status').textContent =
      'Retrying every 2 seconds — ' + attempts + (attempts === 1 ? ' attempt' : ' attempts');
    try {{ window.ipc.postMessage('retry'); }} catch (e) {{}}
  }}, 2000);
</script>
</body>
</html>"#,
        name = escape(&target.name),
        url = escape(&target.url),
        alternatives = if alternatives.is_empty() {
            String::new()
        } else {
            format!("<p>Or switch to another backend:</p><ul>{alternatives}</ul>")
        },
    )
}

/// Config values reach this page as HTML text, so escape them.
fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(name: &str, url: &str) -> Target {
        Target {
            name: name.to_string(),
            url: url.to_string(),
        }
    }

    /// Mirrors what `btoa` produces in the injected script.
    fn base64(bytes: &[u8]) -> String {
        const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for chunk in bytes.chunks(3) {
            let mut block = [0u8; 3];
            block[..chunk.len()].copy_from_slice(chunk);
            let value = u32::from_be_bytes([0, block[0], block[1], block[2]]);
            for index in 0..4 {
                if index <= chunk.len() {
                    out.push(ALPHABET[((value >> (18 - index * 6)) & 0x3F) as usize] as char);
                } else {
                    out.push('=');
                }
            }
        }
        out
    }

    #[test]
    fn base64_round_trips_every_byte_value() {
        let bytes: Vec<u8> = (0..=255).collect();
        for length in [0, 1, 2, 3, 4, 5, 255, 256] {
            let slice = &bytes[..length.min(bytes.len())];
            assert_eq!(
                decode_base64(&base64(slice)).as_deref(),
                Some(slice),
                "len {length}"
            );
        }
    }

    #[test]
    fn a_full_icon_decodes_to_exactly_one_buffer() {
        let pixels = vec![0x7Au8; (ICON_SIZE * ICON_SIZE * 4) as usize];
        assert_eq!(decode_icon(&base64(&pixels)), Some(pixels));
    }

    #[test]
    fn a_wrong_sized_or_corrupt_icon_is_dropped_rather_than_shown() {
        // Right encoding, wrong pixel count.
        assert_eq!(decode_icon(&base64(&[1, 2, 3, 4])), None);
        // Not base64 at all.
        assert_eq!(decode_icon("not base64!"), None);
        // Truncated payload of otherwise the right length.
        let short = base64(&vec![0u8; (ICON_SIZE * ICON_SIZE * 4) as usize]);
        assert_eq!(decode_icon(&short[..short.len() - 8]), None);
    }

    #[test]
    fn background_colours_parse() {
        assert_eq!(parse_background("26,32,44"), Some((26, 32, 44)));
        assert_eq!(parse_background(" 0 , 0 , 0 "), Some((0, 0, 0)));
        assert_eq!(parse_background("255,255,255"), Some((255, 255, 255)));
    }

    #[test]
    fn malformed_background_colours_are_rejected() {
        assert_eq!(parse_background("26,32"), None);
        assert_eq!(parse_background("26,32,44,55"), None);
        // Out of range for a channel, so not a colour we should trust.
        assert_eq!(parse_background("300,0,0"), None);
        assert_eq!(parse_background(""), None);
    }

    #[test]
    fn offline_page_escapes_config_values() {
        let evil = target("<script>alert(1)</script>", "http://x/\"");
        let page = offline_page(&evil, std::slice::from_ref(&evil), Some(0));
        assert!(!page.contains("<script>alert(1)</script>"));
        assert!(page.contains("&lt;script&gt;"));
    }

    #[test]
    fn offline_page_lists_the_other_backends_but_not_the_active_one() {
        let targets = vec![
            target("ComfyUI", "http://127.0.0.1:8188"),
            target("Forge", "http://127.0.0.1:7861"),
        ];
        let page = offline_page(&targets[0], &targets, Some(0));
        assert!(page.contains("Forge"));
        // The active backend appears in the heading, not the switch list.
        assert_eq!(page.matches("ComfyUI").count(), 2);
    }

    #[test]
    fn offline_page_omits_the_switch_list_when_there_is_nowhere_to_switch() {
        let only = target("ComfyUI", "http://127.0.0.1:8188");
        let page = offline_page(&only, std::slice::from_ref(&only), Some(0));
        assert!(!page.contains("Or switch to another backend"));
    }

    #[test]
    fn a_command_line_address_excludes_nothing_from_the_switch_list() {
        let targets = vec![target("ComfyUI", "http://127.0.0.1:8188")];
        let one_off = target("192.168.1.20:8188", "http://192.168.1.20:8188");
        let page = offline_page(&one_off, &targets, None);
        assert!(page.contains("ComfyUI"));
    }
}
