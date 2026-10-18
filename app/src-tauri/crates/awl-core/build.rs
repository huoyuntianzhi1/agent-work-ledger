fn main() {
    // Stage 1-I: link system leveldb for Chromium IndexedDB readers.
    // The connector feature is opt-in: when libleveldb is missing we just
    // skip link-search so the rest of the build still succeeds; the IDB
    // connectors will register themselves as `disabled` at runtime if
    // they cannot open any *.leveldb roots.
    //
    // macOS dev machines should `brew install leveldb`. The CI / shipping
    // story (statically linking leveldb into the .app bundle) is owned by
    // the packaging stage, not by dev.

    let leveldb_dirs: &[(&str, &str)] = &[
        ("/opt/homebrew/lib", "/opt/homebrew/include"),
        ("/usr/local/lib", "/usr/local/include"),
        ("/opt/local/lib", "/opt/local/include"),
    ];

    for (lib_dir, _inc_dir) in leveldb_dirs {
        let candidate = std::path::Path::new(lib_dir).join("libleveldb.dylib");
        if candidate.exists() {
            println!("cargo:rustc-link-search=native={lib_dir}");
            println!("cargo:rustc-link-lib=leveldb");
            break;
        }
    }
}
