//! Restrict the Windows DLL's export table to the engine entry points.
//! (MinGW-style linkers otherwise export every global symbol.)

fn main() {
    println!("cargo:rerun-if-changed=exports.def");
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if os == "windows" && env == "gnu" {
        let def = std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("exports.def");
        println!("cargo:rustc-cdylib-link-arg={}", def.display());
    }
}
