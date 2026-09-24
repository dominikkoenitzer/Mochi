//! Embeds the Mochi icon, so the client carries the same mark as the daemon.

fn main() {
    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        winresource::WindowsResource::new()
            .set_icon("../../assets/mochi.ico")
            .compile()
            .expect("could not embed the icon");
    }
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../../assets/mochi.ico");
}
