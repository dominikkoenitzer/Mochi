//! Embeds the Mochi icon, so the client carries the same mark as the daemon.

fn main() {
    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        // File properties and Task Manager show these, not the crate name.
        winresource::WindowsResource::new()
            .set("FileDescription", "Mochi command line")
            .set("ProductName", "Mochi")
            .set("CompanyName", "Mochi")
            .set("OriginalFilename", "mochic.exe")
            .set("InternalName", "mochic")
            .set("LegalCopyright", env!("CARGO_PKG_LICENSE"))
            .set_icon("../../assets/mochi.ico")
            .compile()
            .expect("could not embed the icon");
    }
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../../assets/mochi.ico");
}
