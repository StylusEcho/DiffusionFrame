//! WebView2 command-line construction.
//!
//! These flags are the difference between "a browser that happens to be small"
//! and a frame that stays out of the way of a running diffusion backend.

/// Chromium features switched off in every mode.
///
/// The first three are wry's own defaults; passing `additional_browser_args`
/// replaces them wholesale, so they have to be repeated here. Chromium only
/// honours the last `--disable-features` on the command line, which is why
/// this is one merged list rather than several flags.
const DISABLED_FEATURES: &[&str] = &[
    // wry defaults: drop the "mini menu" overlay, the out-of-process PDF UI
    // and SmartScreen reporting.
    "msWebOOUI",
    "msPdfOOUI",
    "msSmartScreenProtection",
    // Phone-home and background-work features a local frame has no use for.
    "Translate",
    "OptimizationHints",
    "MediaRouter",
    "AutofillServerCommunication",
];

/// Applied in every mode: no background networking, no crash-reporter process,
/// no component updates, and a single renderer since only one backend is ever
/// on screen.
const LEAN_ARGS: &[&str] = &[
    "--disable-background-networking",
    "--disable-component-update",
    "--disable-breakpad",
    "--disable-sync",
    "--no-first-run",
    "--no-default-browser-check",
    "--renderer-process-limit=1",
];

/// Applied when colour management is off.
///
/// Chromium's default is to convert page colours into the display's ICC
/// profile. On a wide-gamut monitor that makes generated images look more
/// saturated in the frame than in an unmanaged viewer. Pinning the profile to
/// sRGB turns the conversion into a no-op, so pixels reach the screen with the
/// values the backend produced.
const NO_COLOUR_MANAGEMENT: &str = "--force-color-profile=srgb";

/// Applied only when hardware acceleration is off. `--disable-software-rasterizer`
/// keeps SwiftShader from starting as a GPU stand-in, so no GPU process is
/// spawned at all and no video memory is claimed.
const NO_GPU_ARGS: &[&str] = &[
    "--disable-gpu",
    "--disable-gpu-compositing",
    "--disable-gpu-rasterization",
    "--disable-software-rasterizer",
    "--disable-accelerated-2d-canvas",
    "--disable-accelerated-video-decode",
];

pub fn build(hardware_acceleration: bool, colour_management: bool) -> String {
    let mut args = vec![format!(
        "--disable-features={}",
        DISABLED_FEATURES.join(",")
    )];
    args.extend(LEAN_ARGS.iter().map(|arg| arg.to_string()));
    if !hardware_acceleration {
        args.extend(NO_GPU_ARGS.iter().map(|arg| arg.to_string()));
    }
    if !colour_management {
        args.push(NO_COLOUR_MANAGEMENT.to_string());
    }
    args.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accelerated_mode_leaves_the_gpu_alone() {
        let args = build(true, true);
        assert!(!args.contains("--disable-gpu"));
        assert!(args.contains("--disable-background-networking"));
    }

    #[test]
    fn unaccelerated_mode_also_suppresses_the_swiftshader_fallback() {
        let args = build(false, true);
        assert!(args.contains("--disable-gpu "));
        assert!(args.contains("--disable-software-rasterizer"));
    }

    #[test]
    fn only_one_disable_features_flag_is_emitted() {
        // Chromium keeps the last one it sees; more than one silently drops
        // whatever came before it.
        for accelerated in [true, false] {
            assert_eq!(
                build(accelerated, true)
                    .matches("--disable-features")
                    .count(),
                1
            );
        }
    }

    #[test]
    fn colour_management_only_pins_the_profile_when_switched_off() {
        assert!(!build(true, true).contains("--force-color-profile"));
        assert!(build(true, false).contains("--force-color-profile=srgb"));
    }

    #[test]
    fn the_two_toggles_are_independent() {
        let args = build(false, false);
        assert!(args.contains("--disable-gpu "));
        assert!(args.contains("--force-color-profile=srgb"));
    }

    #[test]
    fn wry_defaults_are_carried_over() {
        // Passing additional args replaces wry's defaults, so losing these
        // would quietly reintroduce the mini menu and SmartScreen reporting.
        let args = build(true, true);
        for feature in ["msWebOOUI", "msPdfOOUI", "msSmartScreenProtection"] {
            assert!(args.contains(feature), "missing {feature}");
        }
    }
}
