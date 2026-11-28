fn main() {
    // No CRT: nothing to link against. The DLL's entry point is its DllMain;
    // the exe (src/bin) is a stand-in title executable.
    println!("cargo:rustc-link-arg=/NODEFAULTLIB");
    println!("cargo:rustc-link-arg-cdylib=/ENTRY:probe_dll_main");
    println!("cargo:rustc-link-arg-bins=/ENTRY:probe_exe_main");
    println!("cargo:rustc-link-arg-bins=/SUBSYSTEM:CONSOLE");
}
