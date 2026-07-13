use std::fs;
use std::path::Path;

#[test]
fn ffi_workspace_crate_is_explicitly_unpublished_and_unsupported() {
    let crate_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let manifest = fs::read_to_string(crate_root.join("Cargo.toml")).unwrap();
    let source = fs::read_to_string(crate_root.join("src/lib.rs")).unwrap();
    let readme = fs::read_to_string(crate_root.join("../README.md")).unwrap();

    assert!(manifest.contains("publish = false"));
    assert!(manifest.contains("description = \"Unpublished"));
    assert!(!manifest.contains("cdylib"));
    assert!(!manifest.contains("staticlib"));
    assert!(readme.contains("not published"));
    assert!(readme.contains("does not expose a C ABI"));

    // The crate must stay genuinely API-empty until a binding is designed: no exported
    // Rust items and no foreign-function surface. Guards against re-introducing any
    // placeholder (not just the original `add`), which would defeat the honesty contract.
    for forbidden in [
        "pub fn",
        "pub struct",
        "pub enum",
        "pub extern",
        "extern \"C\"",
        "#[no_mangle]",
        "#[unsafe(no_mangle)]",
    ] {
        assert!(
            !source.contains(forbidden),
            "ffi crate must remain API-empty, but src/lib.rs contains `{forbidden}`"
        );
    }
}
