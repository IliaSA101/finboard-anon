// Иконка и версия в .exe: COFF-объект assets/icon.o с ресурсами (собирает tools/icon_to_coff.py из assets/icon.ico и Cargo.toml)
// передаётся линковщику напрямую — windres/rc не нужны.
fn main() {
    println!("cargo:rerun-if-changed=assets/icon.o");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let res = std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("assets").join("icon.o");
        println!("cargo:rustc-link-arg-bins={}", res.display());
    }
}
