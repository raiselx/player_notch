fn main() {
    println!("cargo:rerun-if-changed=PlayerNotch.ico");
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut resource = winresource::WindowsResource::new();
        resource
            .set_icon("PlayerNotch.ico")
            .set("FileDescription", "PlayerNotch")
            .set("ProductName", "PlayerNotch")
            .set("LegalCopyright", "Copyright (c) 2026 Yerbolat");
        resource
            .compile()
            .expect("Windows resource compiler is required (Windows SDK or MinGW windres)");
    }
}
