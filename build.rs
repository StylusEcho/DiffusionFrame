//! Embeds the Windows executable icon.
//!
//! The icon itself is regenerated from the upstream Material Symbols glyph by
//! `tools/make_icon.py`; this only links the committed `.ico` into the binary
//! so Explorer and the taskbar pick it up.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=assets/icon.rc");
    println!("cargo:rerun-if-changed=assets/icon.ico");

    // Nothing to embed when the target is not Windows.
    if std::env::var_os("CARGO_CFG_WINDOWS").is_none() {
        return;
    }

    match embed_resource::compile("assets/icon.rc", embed_resource::NONE) {
        // A toolchain without a resource compiler still produces a working
        // binary, just with the default icon -- not worth failing the build.
        embed_resource::CompilationResult::NotAttempted(reason) => {
            println!("cargo:warning=icon not embedded: {reason}");
        }
        embed_resource::CompilationResult::Failed(reason) => {
            panic!("failed to embed icon: {reason}");
        }
        _ => {}
    }
}
