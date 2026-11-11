fn main() {
    println!("cargo:rerun-if-changed=assets/michost.ico");
    println!("cargo:rerun-if-changed=build.rs");
    for name in [
        "MICHOST_BUILD_VERSION",
        "MICHOST_BUILD_TIME",
        "MICHOST_GIT_HASH",
        "MICHOST_GIT_DIRTY",
    ] {
        println!("cargo:rerun-if-env-changed={name}");
    }

    let package_version =
        std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".to_string());
    let build_version = std::env::var("MICHOST_BUILD_VERSION")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| package_version.clone());
    let git_hash = std::env::var("MICHOST_GIT_HASH")
        .ok()
        .filter(|value| !value.trim().is_empty());
    let git_dirty = std::env::var("MICHOST_GIT_DIRTY")
        .map(|value| value == "1")
        .unwrap_or(false);
    let build_time = std::env::var("MICHOST_BUILD_TIME")
        .ok()
        .filter(|value| !value.trim().is_empty());
    let display_version = match git_hash.as_deref() {
        Some(hash) if hash != "nogit" => {
            let dirty = if git_dirty { " dirty" } else { "" };
            match build_time.as_deref().and_then(format_build_time) {
                Some(when) => format!("{package_version} ({when}, {hash}{dirty})"),
                None => format!("{package_version} ({hash}{dirty})"),
            }
        }
        _ => package_version.clone(),
    };
    println!("cargo:rustc-env=MICHOST_DISPLAY_VERSION={display_version}");
    println!("cargo:rustc-env=MICHOST_BUILD_VERSION={build_version}");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut resource = winres::WindowsResource::new();
    resource.set_icon("assets/michost.ico");
    resource.set("FileDescription", "MicHost");
    resource.set("ProductName", "MicHost");
    resource.set("OriginalFilename", "michost.exe");
    resource.set("ProductVersion", &build_version);
    if let Err(error) = resource.compile() {
        println!("cargo:warning=embedding the Windows icon failed: {error}");
    }
}

fn format_build_time(value: &str) -> Option<String> {
    if value.len() != 16 || !value.is_ascii() {
        return None;
    }
    let bytes = value.as_bytes();
    if bytes[8] != b'T' || bytes[15] != b'Z' {
        return None;
    }
    Some(format!(
        "{}-{}-{} {}:{} UTC",
        &value[0..4],
        &value[4..6],
        &value[6..8],
        &value[9..11],
        &value[11..13]
    ))
}
