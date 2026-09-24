fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-env-changed=NZ_RUST_UPDATE_MANIFEST_URL");
    if let Ok(url) = std::env::var("NZ_RUST_UPDATE_MANIFEST_URL") {
        println!("cargo:rustc-env=NEZHA_DEFAULT_UPDATE_MANIFEST_URL={url}");
    }
    println!(
        "cargo:rustc-env=NEZHA_BUILD_TARGET={}",
        std::env::var("TARGET")?
    );
    std::env::set_var("PROTOC", protoc_bin_vendored::protoc_bin_path()?);
    tonic_build::compile_protos("proto/nezha.proto")?;
    println!("cargo:rerun-if-changed=proto/nezha.proto");
    Ok(())
}
