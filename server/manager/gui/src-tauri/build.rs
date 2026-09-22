fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rerun-if-changed=src/removal_profile.m");
        cc::Build::new().file("src/removal_profile.m").flag("-fobjc-arc")
            .flag("-fblocks").compile("risunest_sync_removal_profile");
        println!("cargo:rustc-link-lib=framework=WebKit");
    }
    tauri_build::build()
}
