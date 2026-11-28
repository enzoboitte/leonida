fn main() {
    // On macOS a 64-bit executable reserves its first 4 GiB (`__PAGEZERO`),
    // and titles ask for memory there (32-bit graphics memory): keep 64 KiB,
    // as Windows does, and as Wine does on macOS.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-link-arg=-Wl,-pagezero_size,0x10000");
    }
}
