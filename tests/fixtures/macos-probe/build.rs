fn main() {
    // No CRT: nothing to link against, no entry point to run.
    for arg in ["/NODEFAULTLIB", "/NOENTRY"] {
        println!("cargo:rustc-link-arg-cdylib={arg}");
    }
}
