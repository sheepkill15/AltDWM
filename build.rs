fn main() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let manifest_path = format!("{}/alt-dwm.manifest", manifest_dir);
    println!("cargo:rerun-if-changed=alt-dwm.manifest");
    println!("cargo:rerun-if-changed=build.rs");
    // `/MANIFEST:EMBED` is a link.exe switch. Emitting it unconditionally broke
    // the GNU toolchain, which passes it straight to ld and fails to link.
    let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if target_env == "msvc" {
        println!("cargo:rustc-link-arg=/MANIFEST:EMBED");
        println!("cargo:rustc-link-arg=/MANIFESTINPUT:{}", manifest_path);
    } else {
        println!(
            "cargo:warning=manifest not embedded: target env '{}' is not msvc; \
             DPI awareness and the requested execution level will use defaults",
            target_env
        );
    }
}
