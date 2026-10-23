fn main() {
    println!("cargo:rerun-if-changed=../../../src-tauri/windows-app-manifest.xml");
    tauri_build::try_build(tauri_build::Attributes::new().windows_attributes(
        tauri_build::WindowsAttributes::new().app_manifest(include_str!("../../../src-tauri/windows-app-manifest.xml")),
    )).expect("build native boundary resources");
}
