//! Embeds the two Mochi binaries, a manifest and the Mochi icon.
//!
//! MOCHI_SETUP_PAYLOAD names a folder that holds a built mochi.exe and
//! mochic.exe. The release workflow sets it after building and checking them;
//! a build without it still compiles, and the exe it makes says so when it is
//! opened instead of installing anything.
//!
//! The manifest matters more than usual here. Windows treats an exe with
//! "setup" in its name and no requested execution level as an installer and
//! asks for administrator rights before it starts, and Mochi must not be
//! installed or started elevated. It also brings in Common Controls 6, which
//! the task dialog needs.

use embed_manifest::manifest::{DpiAwareness, ExecutionLevel};
use embed_manifest::{embed_manifest, new_manifest};
use std::path::PathBuf;

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-changed=../../assets/mochi.ico");
    println!("cargo::rerun-if-env-changed=MOCHI_SETUP_PAYLOAD");
    println!("cargo::rustc-check-cfg=cfg(payload)");

    if let Some(dir) = std::env::var_os("MOCHI_SETUP_PAYLOAD") {
        let dir = PathBuf::from(dir);
        assert!(
            dir.is_absolute(),
            "MOCHI_SETUP_PAYLOAD must be an absolute path, got {}",
            dir.display()
        );
        for (name, var) in [
            ("mochi.exe", "MOCHI_SETUP_MOCHI"),
            ("mochic.exe", "MOCHI_SETUP_MOCHIC"),
        ] {
            let path = dir.join(name);
            assert!(
                path.is_file(),
                "MOCHI_SETUP_PAYLOAD is {} but there is no {name} in it",
                dir.display()
            );
            println!("cargo::rerun-if-changed={}", path.display());
            println!("cargo::rustc-env={var}={}", path.display());
        }
        println!("cargo::rustc-cfg=payload");
    }

    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        let manifest = new_manifest("Mochi.Setup")
            .dpi_awareness(DpiAwareness::PerMonitorV2)
            .requested_execution_level(ExecutionLevel::AsInvoker);
        embed_manifest(manifest).expect("could not embed the application manifest");
        // Task Manager and the download warning show these, not the crate name.
        winresource::WindowsResource::new()
            .set("FileDescription", "Mochi Setup")
            .set("ProductName", "Mochi")
            .set_icon("../../assets/mochi.ico")
            .compile()
            .expect("could not embed the icon");
    }
}
