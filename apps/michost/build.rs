fn main() {
    println!("cargo:rerun-if-changed=assets/michost.ico");
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut resource = winres::WindowsResource::new();
    resource.set_icon("assets/michost.ico");
    resource.set("FileDescription", "MicHost");
    resource.set("ProductName", "MicHost");
    resource.set("OriginalFilename", "michost.exe");
    if let Err(error) = resource.compile() {
        println!("cargo:warning=embedding the Windows icon failed: {error}");
    }
}
