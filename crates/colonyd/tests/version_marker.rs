//! The app reads colonyd's version from the file without running it; the marker must
//! survive linking. Run with `--release` to check the LTO/strip-free release layout too.

#[test]
fn the_built_binary_carries_its_version() {
    let bytes = std::fs::read(env!("CARGO_BIN_EXE_colonyd")).unwrap();
    assert_eq!(colony_source::daemon_info::version_from_bytes(&bytes).as_deref(), Some(env!("CARGO_PKG_VERSION")));
}

#[test]
fn another_exe_has_no_marker() {
    let me = std::fs::read(std::env::current_exe().unwrap()).unwrap();
    // This test binary links colony-source, so the prefix constant may be in it, but no version.
    assert_eq!(colony_source::daemon_info::version_from_bytes(&me), None);
}
