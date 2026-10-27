fn main() {
    println!(
        "cargo:rustc-env=FERL_BUILD_VERSION={}",
        env!("CARGO_PKG_VERSION")
    );
    println!("cargo:rerun-if-changed=Cargo.toml");
}
