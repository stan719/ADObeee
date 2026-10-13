//! Identifies the running package only when local evidence ties the executable to it.

use std::path::{Path, PathBuf};

use pdfcraft_ui_egui::updates::{Architecture, InstallContext, InstallKind, Platform};

const OFFICIAL_REPOSITORY: &str = "https://github.com/storytold/pdfcraft";
const FLATPAK_INFO: &str = "/.flatpak-info";
const FLATPAK_APP: &str = "/app";
const DEBIAN_QUERY: &str = "/usr/bin/dpkg-query";
const RPM_QUERY: &str = "/usr/bin/rpm";
const LINUX_PACKAGE_EXE: &str = "/usr/bin/pdfcraft";
const MAX_PLIST_SIZE: u64 = 128 * 1024;
const MAX_MARKER_SIZE: u64 = 8 * 1024;
const MAX_PACKAGE_OUTPUT: usize = 4 * 1024;
const LINUX_PACKAGE_QUERY_BUDGET: std::time::Duration = std::time::Duration::from_millis(300);

/// Detects the platform and installation type from the running build and local package evidence.
pub(crate) fn detect() -> InstallContext {
    let platform = current_platform();
    let architecture = current_architecture();
    let kind = if is_development_build(pdfcraft_engine::build_info::LABEL, option_env!("CARGO_PKG_REPOSITORY"), cfg!(debug_assertions))
        || is_fork_workflow(option_env!("GITHUB_REPOSITORY"))
    {
        InstallKind::Development
    } else {
        detected_install_kind(platform)
    };
    InstallContext { platform, architecture, kind }
}

fn current_platform() -> Platform {
    #[cfg(target_os = "windows")]
    {
        Platform::Windows
    }
    #[cfg(target_os = "macos")]
    {
        Platform::Macos
    }
    #[cfg(target_os = "linux")]
    {
        Platform::Linux
    }
    #[cfg(target_os = "freebsd")]
    {
        Platform::Freebsd
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux", target_os = "freebsd")))]
    {
        Platform::Unsupported
    }
}

fn current_architecture() -> Architecture {
    match std::env::consts::ARCH {
        "x86" => Architecture::X86,
        "x86_64" => Architecture::X64,
        "aarch64" => Architecture::Arm64,
        "riscv64" => Architecture::Riscv64,
        _ => Architecture::Unknown,
    }
}

fn is_fork_workflow(repository: Option<&str>) -> bool {
    repository.is_some_and(|repository| repository != "storytold/pdfcraft")
}

fn is_development_build(label: Option<&str>, repository: Option<&str>, debug: bool) -> bool {
    debug || label.is_some_and(|value| !value.trim().is_empty()) || repository != Some(OFFICIAL_REPOSITORY)
}

fn detected_install_kind(platform: Platform) -> InstallKind {
    let Some(executable) = std::env::current_exe().ok().and_then(|path| path.canonicalize().ok()) else {
        return InstallKind::Unknown;
    };
    match platform {
        Platform::Linux => {
            let info = read_small_text(Path::new(FLATPAK_INFO), MAX_MARKER_SIZE);
            linux_install_kind(&executable, info.as_deref())
        }
        Platform::Macos => mac_app_install_kind(&executable),
        Platform::Windows => windows_portable_install_kind(&executable),
        // No package-manager ownership query is made on FreeBSD. A conventional path alone does
        // not prove which package installed the running executable.
        Platform::Freebsd | Platform::Unsupported => InstallKind::Unknown,
    }
}

fn linux_install_kind(executable: &Path, flatpak_info: Option<&str>) -> InstallKind {
    let flatpak = flatpak_install_kind(executable, Path::new(FLATPAK_APP), flatpak_info);
    if flatpak != InstallKind::Unknown {
        return flatpak;
    }
    // These package formats both install this exact path. Skip subprocess work for AppImages,
    // tarballs, development builds, and executables launched from arbitrary directories.
    if executable != Path::new(LINUX_PACKAGE_EXE) {
        return InstallKind::Unknown;
    }

    let deadline = std::time::Instant::now() + LINUX_PACKAGE_QUERY_BUDGET;
    let debian = debian_package_owns(executable, deadline);
    let rpm = rpm_package_owns(executable, deadline);
    package_manager_kind(debian, rpm)
}

fn package_manager_kind(debian: bool, rpm: bool) -> InstallKind {
    match (debian, rpm) {
        (true, false) => InstallKind::Deb,
        (false, true) => InstallKind::Rpm,
        // Neither manager can establish ownership, or both claim it: do not guess.
        _ => InstallKind::Unknown,
    }
}

fn debian_package_owns(executable: &Path, deadline: std::time::Instant) -> bool {
    let query = Path::new(DEBIAN_QUERY);
    if !query.is_file() {
        return false;
    }
    let owner_args = [std::ffi::OsStr::new("-S"), std::ffi::OsStr::new("--"), executable.as_os_str()];
    let Some(owner_output) = run_bounded_query(query, &owner_args, deadline) else { return false };
    if !dpkg_owner_matches(&owner_output, executable) {
        return false;
    }
    let status_args = [std::ffi::OsStr::new("-W"), std::ffi::OsStr::new("-f=${Status}"), std::ffi::OsStr::new("pdfcraft")];
    run_bounded_query(query, &status_args, deadline).is_some_and(|output| dpkg_status_is_installed(&output))
}

fn rpm_package_owns(executable: &Path, deadline: std::time::Instant) -> bool {
    let query = Path::new(RPM_QUERY);
    if !query.is_file() {
        return false;
    }
    let args = [
        std::ffi::OsStr::new("-qf"),
        std::ffi::OsStr::new("--queryformat"),
        std::ffi::OsStr::new("%{NAME}"),
        std::ffi::OsStr::new("--"),
        executable.as_os_str(),
    ];
    run_bounded_query(query, &args, deadline).is_some_and(|output| rpm_owner_matches(&output))
}

/// Runs a fixed system package query without a shell and returns only small, successful output.
fn run_bounded_query(program: &Path, args: &[&std::ffi::OsStr], deadline: std::time::Instant) -> Option<String> {
    use std::io::Read;
    use std::process::{Command, Stdio};

    if std::time::Instant::now() >= deadline {
        return None;
    }
    let mut child = Command::new(program).args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()?;
    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return None;
    };
    // Drain concurrently so a full pipe cannot stall the query. A descendant holding stdout
    // open must not extend the startup deadline after the query process has exited.
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    if std::thread::Builder::new()
        .name("pdfcraft-package-query".into())
        .spawn(move || {
            let mut bytes = Vec::new();
            let result = stdout
                .take(MAX_PACKAGE_OUTPUT.saturating_add(1) as u64)
                .read_to_end(&mut bytes)
                .ok()
                .filter(|_| bytes.len() <= MAX_PACKAGE_OUTPUT)
                .and_then(|_| String::from_utf8(bytes).ok());
            let _ = tx.send(result);
        })
        .is_err()
    {
        let _ = child.kill();
        let _ = child.wait();
        return None;
    }
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(Some(_)) => return None,
            Ok(None) if std::time::Instant::now() < deadline => {
                let sleep = std::time::Duration::from_millis(5).min(deadline.saturating_duration_since(std::time::Instant::now()));
                std::thread::sleep(sleep);
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    rx.recv_timeout(deadline.saturating_duration_since(std::time::Instant::now())).ok().flatten()
}

fn one_line(output: &str) -> Option<&str> {
    let line = output.strip_suffix('\n').unwrap_or(output);
    (!line.contains(['\n', '\r'])).then_some(line)
}

fn dpkg_owner_matches(output: &str, executable: &Path) -> bool {
    let Some(line) = one_line(output) else { return false };
    let Some((package, owned_path)) = line.split_once(": ") else { return false };
    let package_matches = package == "pdfcraft"
        || package
            .strip_prefix("pdfcraft:")
            .is_some_and(|arch| !arch.is_empty() && arch.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'));
    package_matches && Path::new(owned_path) == executable
}

fn dpkg_status_is_installed(output: &str) -> bool {
    one_line(output) == Some("install ok installed")
}

fn rpm_owner_matches(output: &str) -> bool {
    one_line(output) == Some("pdfcraft")
}

fn flatpak_install_kind(executable: &Path, app_root: &Path, flatpak_info: Option<&str>) -> InstallKind {
    let expected_executable = app_root.join("bin").join("pdfcraft");
    let valid_metadata = flatpak_info.is_some_and(|contents| flatpak_application_id(contents).as_deref() == Some("ai.storyteller.pdfcraft"));
    if executable == expected_executable && valid_metadata { InstallKind::Flatpak } else { InstallKind::Unknown }
}

fn flatpak_application_id(contents: &str) -> Option<String> {
    let mut in_application = false;
    let mut found = None;
    for line in contents.lines().map(str::trim) {
        if line.starts_with('[') && line.ends_with(']') {
            in_application = line == "[Application]";
            continue;
        }
        if !in_application {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else { continue };
        if key.trim() == "name" {
            if found.is_some() {
                return None;
            }
            found = Some(value.trim().to_owned());
        }
    }
    found
}

fn mac_app_install_kind(executable: &Path) -> InstallKind {
    let Some(bundle) = pdfcraft_bundle_for_executable(executable) else {
        return InstallKind::Unknown;
    };
    let info_plist = bundle.join("Contents").join("Info.plist");
    let Some(contents) = read_small_text(&info_plist, MAX_PLIST_SIZE) else {
        return InstallKind::Unknown;
    };
    if mac_bundle_matches(executable, &bundle, &contents) { InstallKind::MacApp } else { InstallKind::Unknown }
}

fn pdfcraft_bundle_for_executable(executable: &Path) -> Option<PathBuf> {
    executable.ancestors().find(|path| path.file_name().is_some_and(|name| name == "PdfCraft.app")).map(Path::to_path_buf)
}

fn mac_bundle_matches(executable: &Path, bundle: &Path, plist: &str) -> bool {
    let expected_executable = bundle.join("Contents").join("MacOS").join("PdfCraft");
    if executable != expected_executable {
        return false;
    }
    let Some(plist) = without_xml_comments(plist) else { return false };
    if !plist.trim_start().starts_with("<?xml") || !plist.contains("<plist") || !plist.contains("</plist>") {
        return false;
    }
    [
        ("CFBundleIdentifier", "ai.storyteller.pdfcraft"),
        ("CFBundleName", "PdfCraft"),
        ("CFBundleExecutable", "PdfCraft"),
        ("CFBundlePackageType", "APPL"),
    ]
    .into_iter()
    .all(|(key, value)| plist_string(&plist, key).as_deref() == Some(value))
}

fn plist_string(plist: &str, key: &str) -> Option<String> {
    let key_tag = format!("<key>{key}</key>");
    if plist.match_indices(&key_tag).count() != 1 {
        return None;
    }
    let (_, after_key) = plist.split_once(&key_tag)?;
    let after_key = after_key.trim_start();
    let after_open = after_key.strip_prefix("<string>")?;
    let (value, _) = after_open.split_once("</string>")?;
    if value.is_empty() || value.contains(['<', '&']) {
        return None;
    }
    Some(value.to_owned())
}

fn without_xml_comments(xml: &str) -> Option<String> {
    let mut rest = xml;
    let mut output = String::with_capacity(xml.len());
    while let Some(start) = rest.find("<!--") {
        output.push_str(rest.get(..start)?);
        let after_start = rest.get(start + "<!--".len()..)?;
        let end = after_start.find("-->")?;
        rest = after_start.get(end + "-->".len()..)?;
    }
    output.push_str(rest);
    Some(output)
}

fn windows_portable_install_kind(executable: &Path) -> InstallKind {
    let Some(executable_dir) = executable.parent() else { return InstallKind::Unknown };
    if executable.file_name().is_none_or(|name| name != "pdfcraft.exe") {
        return InstallKind::Unknown;
    }
    let marker_path = executable_dir.join("portable.txt");
    let Some(marker) = read_small_text(&marker_path, MAX_MARKER_SIZE) else { return InstallKind::Unknown };
    let expected_data_dir = executable_dir.join(pdfcraft_ui_egui::portable::DATA_DIR);
    if portable_marker_matches(&marker) && pdfcraft_ui_egui::portable::data_dir().is_some_and(|dir| dir == expected_data_dir) {
        InstallKind::WindowsPortable
    } else {
        InstallKind::Unknown
    }
}

fn portable_marker_matches(contents: &str) -> bool {
    contents.lines().next().map(str::trim) == Some("PdfCraft portable mode")
}

fn read_small_text(path: &Path, max_size: u64) -> Option<String> {
    use std::io::Read;

    // Package evidence must be a regular, bounded file, never a pipe/device.
    let metadata = std::fs::metadata(path).ok()?;
    if !metadata.is_file() || metadata.len() > max_size {
        return None;
    }
    let file = std::fs::File::open(path).ok()?;
    let mut contents = String::new();
    file.take(max_size.saturating_add(1)).read_to_string(&mut contents).ok()?;
    (contents.len() as u64 <= max_size).then_some(contents)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLIST: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
<key>CFBundleName</key><string>PdfCraft</string>
<key>CFBundleExecutable</key><string>PdfCraft</string>
<key>CFBundleIdentifier</key><string>ai.storyteller.pdfcraft</string>
<key>CFBundlePackageType</key><string>APPL</string>
</dict></plist>"#;

    #[test]
    fn labels_source_identity_and_debug_builds_are_development_contexts() {
        assert!(is_development_build(Some("fork"), Some(OFFICIAL_REPOSITORY), false));
        assert!(is_development_build(None, Some("https://github.com/example/pdfcraft"), false));
        assert!(is_development_build(None, Some(OFFICIAL_REPOSITORY), true));
        assert!(!is_development_build(None, Some(OFFICIAL_REPOSITORY), false));
        assert!(is_fork_workflow(Some("example/pdfcraft")));
        assert!(!is_fork_workflow(Some("storytold/pdfcraft")));
        assert!(!is_fork_workflow(None));
    }

    #[test]
    fn flatpak_requires_the_matching_running_executable_and_application_metadata() {
        let app_root = Path::new("/app");
        let executable = app_root.join("bin/pdfcraft");
        let info = "[Application]\nname=ai.storyteller.pdfcraft\n[Instance]\ninstance-id=test";
        assert_eq!(flatpak_install_kind(&executable, app_root, Some(info)), InstallKind::Flatpak);
        assert_eq!(flatpak_install_kind(&executable, app_root, Some("[Application]\nname=other.app")), InstallKind::Unknown);
        assert_eq!(flatpak_install_kind(Path::new("/usr/bin/pdfcraft"), app_root, Some(info)), InstallKind::Unknown);
        assert_eq!(flatpak_install_kind(&executable, app_root, None), InstallKind::Unknown);
        assert_eq!(flatpak_application_id("[Application]\nname=first\nname=second"), None);
    }

    #[test]
    fn mac_app_requires_the_expected_executable_path_and_bundle_identity() {
        let bundle = Path::new("/Applications/PdfCraft.app");
        let executable = bundle.join("Contents/MacOS/PdfCraft");
        assert!(mac_bundle_matches(&executable, bundle, PLIST));
        assert!(!mac_bundle_matches(Path::new("/tmp/pdfcraft"), bundle, PLIST));
        assert!(!mac_bundle_matches(&executable, bundle, &PLIST.replace("ai.storyteller.pdfcraft", "example.fork")));
        assert!(!mac_bundle_matches(&executable, bundle, &PLIST.replace("<string>APPL</string>", "<string>????</string>")));
        assert!(!mac_bundle_matches(&executable, bundle, &PLIST.replace("<key>CFBundleName</key>", "<!-- <key>CFBundleName</key> -->")));
    }

    #[test]
    fn marker_evidence_must_match_the_packaged_windows_portable_marker() {
        assert!(portable_marker_matches("PdfCraft portable mode\nMore details"));
        assert!(!portable_marker_matches("local marker"));
        assert!(!portable_marker_matches(""));
        // Linux AppImage variables alone are intentionally not used to infer a package type.
        assert_eq!(flatpak_install_kind(Path::new("/tmp/.mount_pdfcraft/AppRun"), Path::new("/app"), None), InstallKind::Unknown);
    }

    #[test]
    fn linux_package_ownership_requires_one_exact_manager_and_path_match() {
        let executable = Path::new("/usr/bin/pdfcraft");
        assert!(dpkg_owner_matches("pdfcraft: /usr/bin/pdfcraft\n", executable));
        assert!(dpkg_owner_matches("pdfcraft:amd64: /usr/bin/pdfcraft\n", executable));
        assert!(!dpkg_owner_matches("other: /usr/bin/pdfcraft\n", executable));
        assert!(!dpkg_owner_matches("pdfcraft: /usr/bin/other\n", executable));
        assert!(!dpkg_owner_matches("pdfcraft: /usr/bin/pdfcraft\npdfcraft:amd64: /usr/bin/pdfcraft\n", executable));
        assert!(dpkg_status_is_installed("install ok installed\n"));
        assert!(!dpkg_status_is_installed("deinstall ok config-files\n"));
        assert!(rpm_owner_matches("pdfcraft"));
        assert!(rpm_owner_matches("pdfcraft\n"));
        assert!(!rpm_owner_matches("pdfcraft\npdfcraft\n"));
        assert!(!rpm_owner_matches("other"));
        assert_eq!(package_manager_kind(true, false), InstallKind::Deb);
        assert_eq!(package_manager_kind(false, true), InstallKind::Rpm);
        assert_eq!(package_manager_kind(true, true), InstallKind::Unknown);
        assert_eq!(package_manager_kind(false, false), InstallKind::Unknown);
    }

    #[cfg(unix)]
    #[test]
    fn package_query_failures_timeouts_and_oversized_output_are_unknown() {
        use std::ffi::OsStr;
        use std::time::{Duration, Instant};

        let deadline = Instant::now() + Duration::from_secs(1);
        assert!(run_bounded_query(Path::new("/bin/false"), &[], deadline).is_none());

        let start = Instant::now();
        let timeout_args = [OsStr::new("1")];
        let short_deadline = start + Duration::from_millis(25);
        assert!(run_bounded_query(Path::new("/bin/sleep"), &timeout_args, short_deadline).is_none());
        assert!(start.elapsed() < Duration::from_millis(500), "timeout exceeded the bounded wait: {:?}", start.elapsed());

        let large = "x".repeat(MAX_PACKAGE_OUTPUT.saturating_add(1));
        let output_args = [OsStr::new("%s"), OsStr::new(&large)];
        let output_deadline = Instant::now() + Duration::from_secs(1);
        assert!(run_bounded_query(Path::new("/usr/bin/printf"), &output_args, output_deadline).is_none());
    }

    #[test]
    fn appimage_mount_paths_without_verified_package_identity_remain_unknown() {
        let appimage_executable = Path::new("/tmp/.mount_pdfcraft/usr/bin/pdfcraft");
        assert_eq!(linux_install_kind(appimage_executable, None), InstallKind::Unknown);
    }
}
