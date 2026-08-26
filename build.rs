fn main() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let manifest_path = format!("{}/alt-dwm.manifest", manifest_dir);
    println!("cargo:rerun-if-changed=alt-dwm.manifest");
    println!("cargo:rerun-if-changed=build.rs");
    // Embed manifest via MSVC linker (works for x86_64-pc-windows-msvc)
    // Quote path to handle spaces (e.g., C:\Users\John Doe\...)
    println!("cargo:rustc-link-arg=/MANIFEST:EMBED");
    println!("cargo:rustc-link-arg=\"/MANIFESTINPUT:{}\"", manifest_path);
}
