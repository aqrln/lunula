use std::{fs::File, io::Write, path::Path};

fn main() {
    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR should be set");

    File::create(Path::new(&out_dir).join("link.x"))
        .expect("OUT_DIR should be writeable")
        .write_all(include_bytes!("link.x"))
        .expect("writing $OUTDIR/link.x should succeed");

    println!("cargo::rerun-if-changed=link.x");
    println!("cargo::rustc-link-search={out_dir}");
    println!("cargo::rustc-link-arg-bins=-Tlink.x");
}
