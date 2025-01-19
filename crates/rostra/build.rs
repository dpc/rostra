use std::env;
use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=assets");

    // This should make it possible for distros to override default location.
    let out_dir = PathBuf::from(
        std::env::var_os("ROSTRA_BUILD_OUT_DIR").unwrap_or_else(|| env::var_os("OUT_DIR").unwrap()),
    );
    println!("cargo::rustc-env=ROSTRA_SHARE_DIR={}", out_dir.display());

    let assets_out_dir = out_dir.join("assets");

    std::fs::create_dir_all(&assets_out_dir).expect("Create out assets dir");

    copy_files(&PathBuf::from("assets"), &assets_out_dir);
}

fn copy_files(src_dir: &Path, dst_dir: &Path) {
    for entry in std::fs::read_dir(src_dir).expect("failed to read dir") {
        let entry = entry.expect("failed to read entry");
        let path = entry.path();
        let src = path.clone();
        let src_rel = path.strip_prefix(src_dir).expect("Must have prefix");
        let dst = dst_dir.join(src_rel);

        println!("Copying {} to {}", src.display(), dst.display());
        if entry.file_type().unwrap().is_dir() {
            copy_files(&src, &dst);
        } else {
            std::fs::copy(src, dst).expect("failed to copy file");
        }
    }
}
