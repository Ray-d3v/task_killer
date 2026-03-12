fn main() {
    if std::env::var_os("CARGO_CFG_WINDOWS").is_none() {
        return;
    }

    let version = std::env::var("CARGO_PKG_VERSION").expect("package version");
    let mut resource = winresource::WindowsResource::new();
    resource.set("CompanyName", "Ray-d3v");
    resource.set("ProductName", "Task Killer");
    resource.set("FileDescription", "Task Killer privileged backend service");
    resource.set("ProductVersion", &version);
    resource.set("FileVersion", &version);
    resource.set_version_info(
        winresource::VersionInfo::PRODUCTVERSION,
        parse_version_words(&version),
    );
    resource.set_version_info(
        winresource::VersionInfo::FILEVERSION,
        parse_version_words(&version),
    );
    resource
        .compile()
        .unwrap_or_else(|error| panic!("compile tasktui-service resources: {error}"));
}

fn parse_version_words(version: &str) -> u64 {
    let mut parts = version
        .split('.')
        .map(|value| value.parse::<u16>().unwrap_or(0))
        .collect::<Vec<_>>();
    while parts.len() < 4 {
        parts.push(0);
    }
    ((parts[0] as u64) << 48)
        | ((parts[1] as u64) << 32)
        | ((parts[2] as u64) << 16)
        | parts[3] as u64
}
