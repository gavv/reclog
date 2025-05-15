use cargo_toml::Manifest;
use sha2::{Digest, Sha256};
use std::env;
use std::fs::{self, File};
use std::io::Write;
use std::path::Path;
use std::process::Command;

fn build_temp_crate(crate_deps: &[(&str, &str)], rust_edition: &str, rust_code: &str) -> bool {
    let out_dir = env::var("OUT_DIR").unwrap();

    let crate_name = format!("reclog_{}", hex::encode(Sha256::digest(rust_code)));
    let crate_dir = Path::new(&out_dir).join(&crate_name);

    fs::create_dir_all(&crate_dir).unwrap();

    {
        let mut cargo_toml = File::create(crate_dir.join("Cargo.toml")).unwrap();

        writeln!(&mut cargo_toml, "[package]").unwrap();
        writeln!(&mut cargo_toml, "name = \"{}\"", crate_name).unwrap();
        writeln!(&mut cargo_toml, "edition = \"{}\"", rust_edition).unwrap();

        writeln!(&mut cargo_toml, "[dependencies]").unwrap();
        for (dep_name, dep_ver) in crate_deps {
            writeln!(&mut cargo_toml, "{0} = \"{1}\"", dep_name, dep_ver).unwrap();
        }
    }

    fs::create_dir_all(crate_dir.join("src")).unwrap();
    fs::write(crate_dir.join("src/main.rs"), rust_code).unwrap();

    let mut cmd = Command::new(env::var("CARGO").unwrap_or("cargo".to_string()));
    cmd.current_dir(&crate_dir);
    cmd.arg("check");

    if let Ok(value) = env::var("TARGET") {
        cmd.arg("--target").arg(value);
    }

    if let Ok(value) = env::var("RUSTC") {
        cmd.env("RUSTC", value);
    }

    if let Ok(value) = env::var("RUSTFLAGS") {
        cmd.env("RUSTFLAGS", value);
    }

    eprintln!("Running command: {:?}", cmd);
    let status = cmd.status().unwrap();

    eprintln!("Status: {:?}", status);
    status.success()
}

fn check_crate_symbol(manifest: &Manifest, crate_name: &str, symbol: &str) {
    let found = build_temp_crate(
        &[(
            crate_name,
            manifest.dependencies.get(crate_name).unwrap().req(),
        )],
        manifest.package().edition().to_string().as_str(),
        format!(
            r#"
            #[allow(unused_imports)]
            use {0}::{1};
            fn main() {{}}
            "#,
            crate_name, symbol,
        )
        .as_str(),
    );

    eprintln!(
        "Symbol {}::{} {}",
        crate_name,
        symbol,
        if found { "found" } else { "not found" }
    );

    let cfg = symbol.to_lowercase();

    println!("cargo::rustc-check-cfg=cfg(has_{})", cfg);
    if found {
        println!("cargo::rustc-cfg=has_{}", cfg);
    }
}

fn main() {
    // re-run only if build.rs changed
    println!("cargo::rerun-if-changed=build.rs");

    let manifest = Manifest::from_path(env::var("CARGO_MANIFEST_PATH").unwrap()).unwrap();

    check_crate_symbol(&manifest, "libc", "pthread_sigmask");
    check_crate_symbol(&manifest, "libc", "sigtimedwait");
    check_crate_symbol(&manifest, "libc", "timer_create");
    check_crate_symbol(&manifest, "libc", "setitimer");
}
