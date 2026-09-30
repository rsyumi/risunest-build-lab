fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        cc::Build::new().file("src/termination_probe.m").flag("-fobjc-arc").compile("termination_probe");
        println!("cargo:rerun-if-changed=src/termination_probe.m");
        println!("cargo:rustc-link-lib=framework=AppKit");
    }
    tauri_build::build();
}
