//! Embeds an application manifest and the Mochi icon.
//!
//! Per-monitor v2 DPI awareness has to be set before the process creates its
//! first window, and the only way to guarantee that is the manifest. Calling
//! `SetProcessDpiAwarenessContext` at startup is a fallback, not a substitute:
//! on a mixed DPI desktop a system-aware process is handed scaled, lying
//! rectangles by `GetWindowRect`, which is exactly what a tiling manager must
//! not have. `longPathAware` comes along for `QueryFullProcessImageNameW`.

use embed_manifest::manifest::{DpiAwareness, ExecutionLevel};
use embed_manifest::{embed_manifest, new_manifest};

fn main() {
    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        let manifest = new_manifest("Mochi.WindowManager")
            .dpi_awareness(DpiAwareness::PerMonitorV2)
            // As invoker, never a demand for administrator rights: that would
            // put a prompt in front of every login for everyone. A user who
            // wants the windows of administrator programs tiled sets
            // `"elevated": true` in mochi.json, and `mochic start` asks for
            // the rights then, through the prompt Windows shows for it.
            .requested_execution_level(ExecutionLevel::AsInvoker);
        embed_manifest(manifest).expect("could not embed the application manifest");
        winresource::WindowsResource::new()
            .set_icon("../../assets/mochi.ico")
            .compile()
            .expect("could not embed the icon");
    }
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../../assets/mochi.ico");
}
