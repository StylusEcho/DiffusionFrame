# DiffusionFrame

A lean desktop frame for ComfyUI, Stable Diffusion WebUI, Forge, SwarmUI and
InvokeAI.

It gives those web UIs their own window instead of a browser tab, without
bringing a second browser engine along to do it. On Windows it hosts the
WebView2 runtime that is already part of the OS, so there is no bundled
Chromium, no extension host, no updater service, and no background tabs.

The point is what it *doesn't* do: a diffusion backend needs your GPU and your
CPU, and the UI in front of it should not be competing for either.

## What keeps it out of the way

**A hardware acceleration toggle.** `Ctrl+Shift+G`, or `hardware_acceleration`
in the config. With acceleration off, DiffusionFrame starts WebView2 with the
GPU disabled and the SwiftShader fallback suppressed, so no GPU process is
spawned and no video memory is claimed — all of it stays with your model.
Rendering falls back to the CPU, which costs some smoothness on large node
graphs and is usually invisible on everything else. The window title shows
`GPU off` when acceleration is disabled.

Changing this restarts the app, because WebView2 reads its command line once
when the browser environment is created. The restart is handled for you:
window position, zoom, active backend and any command-line address all carry
across.

**Below-normal process priority** (`low_priority`), so the frame yields to
sampling rather than competing with it. Below-normal rather than idle — idle
priority makes the UI stutter badly during generation.

**Idle throttling** (`idle_throttle`). Minimizing hides the webview, which
stops compositing outright, and hints to WebView2 that it can release renderer
caches. Both are handed back on restore.

**No busy work.** The event loop blocks rather than polls, so an idle window
uses no CPU. Background networking, component updates, the crash reporter,
translation, autofill and media routing are all switched off at startup, and
renderers are capped at one since only one backend is ever on screen.

**Links open in their own frame window.** Anything the page opens as a new
page — docs, a model card — gets a DiffusionFrame window rather than replacing
what you were looking at. Those windows share the parent's browser process, so
a second window costs a renderer rather than a whole browser.

## The window menu

Right-click the title bar (or press `Alt+Space`) for the system menu. Below the
usual Move/Size/Close entries:

| Item | |
| --- | --- |
| **Refresh page** | Reloads. On the main window it re-probes the backend first, so refreshing after a shutdown lands on the placeholder rather than a Chromium error page. |
| **Enable/Disable colour management** | Whether the webview converts page colours into your display's ICC profile. |
| **Enable/Disable hardware acceleration** | Same toggle as `Ctrl+Shift+G`. |

Both toggles restart the app, and their menu labels say so. This is not a
shortcut worth apologising for — it is the only way it can work. WebView2 reads
its command line exactly once, when the browser environment is created, and
neither GPU use nor colour conversion is adjustable afterwards through any
runtime API. The restart carries across your window position and size, zoom
level, active backend and any command-line address, so in practice the window
blinks and comes back where it was.

### Colour management

Chromium's default is to convert page colours into your display's ICC profile.
On a wide-gamut monitor that makes a generated image look more saturated inside
the frame than in an unmanaged viewer — the pixels the backend produced are not
the pixels you are seeing. Turning colour management off pins the profile to
sRGB, which makes the conversion a no-op and puts the original values on screen.

On an ordinary sRGB monitor the two settings look identical. Leave it on unless
you are colour-matching against another application.

## Title bar and icon

**The window icon follows the page.** Each window adopts the favicon of
whatever it is showing, so the ComfyUI window carries ComfyUI's icon and a link
window carries the linked site's — handy once several are open in the taskbar.
DiffusionFrame's own icon is the fallback when a page has none.

No image decoder was added for this. The page draws its own favicon onto a
canvas and hands back raw pixels, so whatever format the site uses — `.ico`,
PNG, SVG — is decoded by the browser engine that is already running. A
cross-origin favicon without CORS headers taints the canvas and is skipped,
which leaves the default icon in place.

**The title bar is black by default**, matching the dark UIs these backends
ship with. Set `titlebar` to change it:

| `titlebar` | |
| --- | --- |
| `black` (default) | Black caption, light title text. |
| `page` | Follows the background colour of the page on screen, with the title text flipped to stay legible. Tracks in-page theme switches, not just page loads. |
| `system` | Whatever Windows would do on its own. |

Painting the caption needs Windows 11 (build 22000). On Windows 10 those calls
fail harmlessly and only the dark-mode flag applies, which still gives you a
dark caption — just not an exact colour match.

## Requirements

Windows 10 or 11 with the WebView2 runtime. Windows 11 ships with it; on
Windows 10 it arrives with recent Edge updates, or you can install the
[Evergreen Runtime][webview2] directly.

[webview2]: https://developer.microsoft.com/microsoft-edge/webview2/

## Build

```
cargo build --release
```

The binary lands at `target/release/diffusionframe.exe` and needs no
installation — put it anywhere and run it.

## Usage

```
diffusionframe [ADDRESS]
diffusionframe --url <ADDRESS>
```

With no argument it opens the backend selected in the config (ComfyUI on
`http://127.0.0.1:8188` by default).

`ADDRESS` accepts whichever form is least typing:

```
diffusionframe 8188                     # port on 127.0.0.1
diffusionframe :8188                    # same
diffusionframe 192.168.1.20:8188        # a box on your network
diffusionframe http://127.0.0.1:8188    # full URL; https and [::1] also work
```

An address on the command line overrides the configured backend **for that run
only** and is never written to the config — so a shortcut pointing at a remote
machine cannot quietly become your default. If the address matches a backend
you already have configured, that entry is selected by name instead.

Also: `-h`/`--help`, `-V`/`--version`.

### If the backend isn't running yet

DiffusionFrame checks whether anything is listening before it navigates. If
nothing is, you get a placeholder instead of a browser error page, and it keeps
checking — start ComfyUI afterwards and the frame connects on its own. Launch
order doesn't matter.

## Shortcuts

Everything is behind `Ctrl+Shift` so it stays clear of the backends' own
bindings (ComfyUI uses plain `Ctrl+S`, `Ctrl+O`, `Ctrl+Z` and friends). Keys
that aren't listed here are passed straight through to the page.

| Shortcut | Action |
| --- | --- |
| `Ctrl+Shift+G` | Toggle hardware acceleration (restarts) |
| `Ctrl+Shift+1`…`9` | Switch to the nth configured backend |
| `Ctrl+Shift+R` | Reconnect to the current backend |
| `Ctrl+Shift+F` | Fullscreen |
| `Ctrl+Shift+O` | Open the config folder |
| `Ctrl+Shift+=` / `-` / `0` | Zoom in / out / reset (remembered) |

`Ctrl` with the mouse wheel zooms as well.

## Configuration

`%APPDATA%\DiffusionFrame\config.txt`, created on first run and rewritten on
exit. `Ctrl+Shift+O` opens the folder.

```ini
hardware_acceleration = true
colour_management = true
low_priority = true
idle_throttle = true

titlebar = black

zoom = 1.0
active = 0

window_width = 1440
window_height = 900
window_maximized = false

target = ComfyUI | http://127.0.0.1:8188
target = A1111 WebUI | http://127.0.0.1:7860
target = Forge | http://127.0.0.1:7861
target = SwarmUI | http://127.0.0.1:7801
target = InvokeAI | http://127.0.0.1:9090
```

`target` lines are `Name | URL`, in the order `Ctrl+Shift+1`…`9` selects them;
add as many as you like and point them anywhere, including other machines.
`active` is the zero-based index of the one opened at startup.

The file is rewritten on exit, so comments you add to it are not preserved.
Unknown keys are ignored and malformed values fall back to their defaults —
a hand-edited file can't stop the app from starting.

WebView2 keeps its profile in `%APPDATA%\DiffusionFrame\webview2`. It is shared
between acceleration modes deliberately, so toggling the GPU never discards
saved workflows, logins or UI settings. One consequence: two copies of
DiffusionFrame running at once with *different* acceleration settings will
conflict over that folder, and the second one will report that it can't start.
Two copies with the same setting are fine.

## The icon

DiffusionFrame's own icon — the executable's icon in Explorer, and the fallback
for any window whose page has no favicon — is the **iframe** glyph from
[Material Symbols][symbols]. It is not hand-traced: `tools/make_icon.py` fetches the upstream SVG, rasterizes it with a
small standard-library-only renderer, and writes both `assets/icon.ico` (for the
executable) and `assets/icon-32.rgba` (for the window, so the binary carries no
image decoder). Re-run it to regenerate:

```
python3 tools/make_icon.py
```

[symbols]: https://fonts.google.com/icons

## Development

The logic — config, argument parsing, the reachability probe, the WebView2
command line, the offline page — lives in the library half of the crate and
depends on nothing platform-specific, so it tests anywhere:

```
cargo test --lib --no-default-features
```

The full app needs a system webview. On a machine without one you can still
typecheck the Windows build:

```
rustup target add x86_64-pc-windows-msvc
cargo clippy --target x86_64-pc-windows-msvc --all-targets
```

## License

MIT.
