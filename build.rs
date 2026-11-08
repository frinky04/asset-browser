//! Compiles the icon into the Windows executable, for Explorer and shortcuts.

fn main() {
    println!("cargo:rerun-if-changed=assets/app.rc");
    println!("cargo:rerun-if-changed=assets/icon.ico");
    embed_resource::compile("assets/app.rc", embed_resource::NONE).manifest_optional().expect("compile the app icon");
}
