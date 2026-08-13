//! The two pieces of front-end DiffusionFrame owns: the shortcut bridge
//! injected into the backend's own UI, and the offline placeholder shown when
//! the backend is not listening yet.

use crate::config::Target;

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
