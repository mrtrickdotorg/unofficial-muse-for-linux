//! Compile the bundled icon set into a GResource the binary carries.
//! The rail paints its glyphs from these resources, so the menu needs
//! no files beside the binary.

fn main() {
    let manifest =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/muse.gresource.xml");
    for entry in [
        "muse.gresource.xml",
        "icons/menu.svg",
        "icons/chat.svg",
        "icons/search.svg",
        "icons/library.svg",
        "icons/ideas.svg",
        "icons/tasks.svg",
        "icons/shapes.svg",
        "icons/monitor.svg",
        "icons/new.svg",
        "icons/download.svg",
    ] {
        println!("cargo:rerun-if-changed=assets/{}", entry);
    }
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    let assets = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets");
    let status = std::process::Command::new("glib-compile-resources")
        .arg(format!("--target={}/muse.gresource", out.display()))
        .arg(format!("--sourcedir={}", assets.display()))
        .arg(&manifest)
        .status()
        .expect("glib-compile-resources runs");
    assert!(status.success(), "glib-compile-resources failed");
}
