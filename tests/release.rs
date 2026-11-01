//! Выпуск: версия в свойствах exe.

/// Версия в свойствах exe («Подробно») берётся из ресурса `assets/icon.o`. Его пересобирает
/// `tools/icon_to_coff.py` (и `tools/release.ps1`) — после смены версии в Cargo.toml он не должен отставать.
#[test]
fn exe_resources_carry_current_version() {
    let obj = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/icon.o")).unwrap();
    let utf16 = |s: &str| -> Vec<u8> { s.encode_utf16().flat_map(u16::to_le_bytes).collect() };
    let find = |needle: &[u8], from: usize| obj[from..].windows(needle.len()).position(|w| w == needle).map(|p| p + from);
    for key in ["FileVersion", "ProductVersion"] {
        let at = find(&utf16(key), 0).unwrap_or_else(|| panic!("в ресурсах нет {key} — запустите python tools/icon_to_coff.py"));
        let value = find(&utf16(&format!("{}\0", env!("CARGO_PKG_VERSION"))), at);
        assert!(value.is_some_and(|v| v - at < 40), "{key} в assets/icon.o не {} — запустите python tools/icon_to_coff.py", env!("CARGO_PKG_VERSION"));
    }
}
