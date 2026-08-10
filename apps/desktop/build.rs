fn main() {
    if std::env::var_os("CARGO_CFG_TARGET_OS").as_deref() == Some(std::ffi::OsStr::new("windows")) {
        println!("cargo:rerun-if-changed=resources/windows/icon.rc");
        println!("cargo:rerun-if-changed=assets/app-icons/icon.ico");
        embed_resource::compile("resources/windows/icon.rc", embed_resource::NONE)
            .manifest_optional()
            .expect("failed to embed the Vibex Windows application icon");
    }
}
