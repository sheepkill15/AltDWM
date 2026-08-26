fn main() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let manifest_path = format!("{}/alt-dwm.manifest", manifest_dir);
    println!("cargo:rerun-if-changed=alt-dwm.manifest");
    println!("cargo:rerun-if-changed=build.rs");
    // Embed manifest via MSVC linker (works for x86_64-pc-windows-msvc)
    // Pass the switch as one argument. Cargo/rustc already preserves it as one
    // linker argument; embedded quotes are interpreted literally by mt.exe.
    println!("cargo:rustc-link-arg=/MANIFEST:EMBED");
    println!("cargo:rustc-link-arg=/MANIFESTINPUT:{}", manifest_path);
}
