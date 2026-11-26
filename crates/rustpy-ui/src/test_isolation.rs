//! Unit tests never touch the user's configuration or clipboard (see
//! `rustpy_ui_base::test_support`).

rustpy_ui_base::unit_test_isolation!();

#[test]
fn settings_saves_go_to_a_private_directory_by_default() {
    use rustpy_ui_base::test_support::{config_dir, is_unit_test, isolated};
    let dir = rustpy_settings::Settings::config_dir();
    if isolated() {
        assert_eq!(dir, config_dir(std::process::id()));
    }
    let real = dirs::config_dir().map(|d| d.join(rustpy_settings::APP_DIR_NAME));
    assert_ne!(Some(dir), real, "tests must not write the user's settings");
    assert!(is_unit_test(), "the system clipboard stays out of reach");
}
