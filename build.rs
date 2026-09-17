use std::{env, fs, path::PathBuf};

fn main() {
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let directory = root.join("target/helpers");
    for path in [
        "helper/sericon-helper.c",
        "helper/build.py",
        "target/helpers",
    ] {
        println!("cargo:rerun-if-changed={path}");
    }
    let mut stamp = fs::read(root.join("helper/sericon-helper.c")).unwrap();
    stamp.extend(fs::read(root.join("helper/build.py")).unwrap());
    let mut generated = String::from("pub const BUNDLED: &[(&str, &[u8])] = &[\n");
    let mut count = 0;
    for arch in ["x86_64", "mipsel", "mips", "aarch64", "arm"] {
        let binary = directory.join(arch);
        if !binary.exists() {
            continue;
        }
        assert_eq!(
            fs::read(directory.join(format!("{arch}.stamp")))
                .ok()
                .as_deref(),
            Some(stamp.as_slice()),
            "Stale/missing helper stamp: run python3 helper/build.py --arch {arch}"
        );
        generated.push_str(&format!("({arch:?}, include_bytes!({:?})),\n", binary));
        count += 1;
    }
    generated.push_str("];\n");
    if count == 0 {
        println!(
            "cargo:warning=No DUT helpers bundled; run python3 helper/build.py before building a full release"
        );
    }
    fs::write(
        PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("helper-bundle.rs"),
        generated,
    )
    .unwrap();
}
