fn main() {
    println!("cargo:rerun-if-env-changed=RUSTPY_BUILD_REVISION");
    println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");
    for (source, destination) in [
        ("TARGET", "RUSTPY_BUILD_TARGET"),
        ("PROFILE", "RUSTPY_BUILD_PROFILE"),
        ("RUSTPY_BUILD_REVISION", "RUSTPY_BUILD_REVISION"),
        ("SOURCE_DATE_EPOCH", "RUSTPY_BUILD_EPOCH"),
    ] {
        if let Ok(value) = std::env::var(source) {
            if !value.chars().any(char::is_control) {
                println!("cargo:rustc-env={destination}={value}");
            }
        }
    }
}
