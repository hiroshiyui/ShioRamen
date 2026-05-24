use std::{env, path::PathBuf, process::Command};

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());

    // -------------------------------------------------------------------------
    // mRuby
    // -------------------------------------------------------------------------
    let mruby_dir = manifest.join("vendor/mruby");
    let lib_path = mruby_dir.join("build/host/lib/libmruby.a");

    // Copy shio-specific mruby build configs from the shio repo into the
    // vendor/mruby/build_config/ directory before invoking rake.  These files
    // are intentionally kept outside the submodule so the vendored mruby tree
    // stays pristine at its tagged commit.
    let configs_src = manifest.join("mruby_configs");
    let configs_dst = mruby_dir.join("build_config");
    for name in &["shio.rb", "mcp_safe.gembox"] {
        std::fs::copy(configs_src.join(name), configs_dst.join(name)).unwrap_or_else(|e| {
            panic!("failed to copy mruby_configs/{name} → vendor/mruby/build_config/{name}: {e}")
        });
    }

    // Build mRuby with its own build system (requires `rake`).
    // Skipped if the library already exists to keep incremental builds fast.
    //
    // Uses the shio-specific build config (build_config/shio.rb) which
    // restricts the gembox to stdlib + math, removing the filesystem / network
    // / eval attack surface.  To force a rebuild after changing the gembox:
    //   rm vendor/mruby/build/host/lib/libmruby.a && cargo build
    //
    // mruby's Rakefile resolves MRUBY_CONFIG via:
    //   "#{MRUBY_ROOT}/build_config/#{MRUBY_CONFIG}.rb"
    // so pass only the bare name ("shio"), not the full relative path.
    if !lib_path.exists() {
        let status = Command::new("rake")
            .current_dir(&mruby_dir)
            .env("MRUBY_CONFIG", "shio")
            .status()
            .expect(
                "failed to run `rake` — is Ruby (with rake) installed? \
                 Run `gem install rake` if missing.",
            );
        assert!(status.success(), "mRuby build failed");
    }

    // Link libmruby.a.
    println!(
        "cargo:rustc-link-search=native={}",
        mruby_dir.join("build/host/lib").display()
    );
    println!("cargo:rustc-link-lib=static=mruby");
    println!("cargo:rustc-link-lib=m"); // mRuby uses libm on Linux

    // Compile our C glue shims.
    cc::Build::new()
        .file("src/ruby/glue.c")
        .include(mruby_dir.join("include"))
        .include(mruby_dir.join("build/host/include"))
        .compile("shio_ruby_glue");

    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/ruby/glue.c");
    println!("cargo:rerun-if-changed=mruby_configs/shio.rb");
    println!("cargo:rerun-if-changed=mruby_configs/mcp_safe.gembox");
}
