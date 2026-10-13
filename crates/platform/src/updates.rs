//! Bounded parsing and local package selection for user initiated release checks.

use semver::Version;
use serde::Deserialize;
use serde_json::Value;
use url::Url;

/// The page listing every PdfCraft release.
pub const RELEASES_PAGE: &str = "https://github.com/storytold/pdfcraft/releases";
const MAX_BODY: usize = 1 << 20;
const MAX_NOTES: usize = 64 << 10;
const MAX_ASSETS: usize = 128;
const MAX_ASSET_NAME: usize = 256;
const MAX_URL: usize = 2048;
const MAX_DIGEST: usize = 140;

#[derive(Deserialize)]
struct ReleaseResponse {
    tag_name: String,
    html_url: String,
    body: Option<String>,
    draft: bool,
    prerelease: bool,
    assets: Vec<ReleaseAsset>,
}

#[derive(Deserialize)]
struct ReleaseAsset {
    name: String,
    size: u64,
    browser_download_url: String,
    digest: Option<String>,
}

/// Release metadata returned by GitHub.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub version: String,
    pub url: String,
    pub notes: String,
    pub draft: bool,
    pub prerelease: bool,
    pub assets: Vec<Asset>,
}

/// A downloadable release asset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    pub name: String,
    pub size: u64,
    pub url: String,
    /// GitHub's advertised digest, retained as metadata and never treated as authentication.
    pub digest: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Windows,
    Macos,
    Linux,
    Freebsd,
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Architecture {
    X86,
    X64,
    Arm64,
    Riscv64,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallKind {
    WindowsMsi,
    WindowsPortable,
    MacApp,
    AppImage,
    Deb,
    Rpm,
    Flatpak,
    Unknown,
    Development,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InstallContext {
    pub platform: Platform,
    pub architecture: Architecture,
    pub kind: InstallKind,
}

impl Default for InstallContext {
    fn default() -> Self {
        Self { platform: Platform::Unsupported, architecture: Architecture::Unknown, kind: InstallKind::Unknown }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageSelection<'a> {
    Package(&'a Asset),
    Managed(InstallKind),
    Unknown,
    Missing,
    Ambiguous,
}

/// Parses a GitHub release response, rejecting malformed or unbounded metadata.
pub fn parse_release(body: &str) -> Result<Release, String> {
    if body.len() > MAX_BODY {
        return Err("release response is too large".into());
    }
    let index: Value = serde_json::from_str(body).map_err(|e| format!("unreadable release response ({e})"))?;
    let object = index.as_object().ok_or("release response must be an object")?;
    if !object.contains_key("body") {
        return Err("missing or invalid body".into());
    }
    let indexed_assets = object.get("assets").and_then(Value::as_array).ok_or("assets must be an array")?;
    if indexed_assets.len() > MAX_ASSETS {
        return Err("release has too many assets".into());
    }
    // Typed deserialization rejects duplicate known keys while ignoring newer GitHub fields.
    let raw: ReleaseResponse = serde_json::from_str(body).map_err(|e| format!("unreadable release response ({e})"))?;
    let version = raw.tag_name;
    if version.len() > 64 {
        return Err("tag_name is too long".into());
    }
    let parsed_tag = parse_version(&version).ok_or("release tag is not a valid SemVer version")?;
    let draft = raw.draft;
    let prerelease = raw.prerelease;
    if !parsed_tag.pre.is_empty() && !prerelease {
        return Err("prerelease tag is marked as a stable release".into());
    }
    let notes = raw.body.unwrap_or_default();
    if notes.len() > MAX_NOTES {
        return Err("body is too long".into());
    }
    let url = raw.html_url;
    if url.len() > MAX_URL {
        return Err("html_url is too long".into());
    }
    validate_release_url(&url, &version)?;

    let mut assets = Vec::with_capacity(raw.assets.len());
    let mut names = std::collections::HashSet::with_capacity(raw.assets.len());
    for raw in raw.assets {
        let name = raw.name;
        if name.len() > MAX_ASSET_NAME {
            return Err("asset name is too long".into());
        }
        if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\\', '%']) || name.chars().any(char::is_control) {
            return Err("asset name is unsafe".into());
        }
        if !names.insert(name.clone()) {
            return Err("release contains duplicate asset names".into());
        }
        let size = raw.size;
        if size == 0 {
            return Err("asset size must be a positive integer".into());
        }
        let asset_url = raw.browser_download_url;
        if asset_url.len() > MAX_URL {
            return Err("asset download URL is too long".into());
        }
        validate_asset_url(&asset_url, &version, &name)?;
        let digest = match raw.digest {
            None => None,
            Some(value) if value.len() <= MAX_DIGEST && safe_digest(&value) => Some(value),
            _ => return Err("asset digest is invalid".into()),
        };
        assets.push(Asset { name, size, url: asset_url, digest });
    }
    Ok(Release { version, url, notes, draft, prerelease, assets })
}

/// Whether `latest` has higher SemVer precedence than `current`; build metadata is ignored.
pub fn is_newer(latest: &str, current: &str) -> bool {
    match (parse_version(latest), parse_version(current)) {
        (Some(latest), Some(current)) => latest.cmp_precedence(&current).is_gt(),
        _ => false,
    }
}

/// Selects the unique release file matching this exact installation type and architecture.
pub fn select_package(release: &Release, context: InstallContext) -> PackageSelection<'_> {
    match context.kind {
        InstallKind::Development | InstallKind::Unknown => return PackageSelection::Unknown,
        InstallKind::Flatpak | InstallKind::Deb | InstallKind::Rpm => {
            return if context.platform == Platform::Linux { PackageSelection::Managed(context.kind) } else { PackageSelection::Unknown };
        }
        _ => {}
    }
    let tag = release.version.as_str();
    let version = tag.strip_prefix(['v', 'V']).unwrap_or(tag);
    if tag.len() > 64 || parse_version(tag).is_none() || !valid_release_url(&release.url, tag) {
        return PackageSelection::Unknown;
    }
    let (platform, suffix) = match context.platform {
        Platform::Windows => (
            "windows",
            match context.architecture {
                Architecture::X86 => "x86",
                Architecture::X64 => "x64",
                Architecture::Arm64 => "arm64",
                _ => return PackageSelection::Unknown,
            },
        ),
        Platform::Macos => (
            "macos",
            match context.architecture {
                Architecture::X64 | Architecture::Arm64 => "universal",
                _ => return PackageSelection::Unknown,
            },
        ),
        Platform::Linux => (
            "linux",
            match context.architecture {
                Architecture::X86 => "i686",
                Architecture::X64 => "x86_64",
                Architecture::Arm64 => "aarch64",
                Architecture::Riscv64 => "riscv64",
                Architecture::Unknown => return PackageSelection::Unknown,
            },
        ),
        Platform::Freebsd => (
            "freebsd",
            match context.architecture {
                Architecture::X64 => "x86_64",
                Architecture::Arm64 => "aarch64",
                _ => return PackageSelection::Unknown,
            },
        ),
        Platform::Unsupported => return PackageSelection::Unknown,
    };
    let extension = match (context.platform, context.kind) {
        (Platform::Windows, InstallKind::WindowsMsi) => "msi",
        (Platform::Windows, InstallKind::WindowsPortable) => "-portable.zip",
        (Platform::Macos, InstallKind::MacApp) => "dmg",
        (Platform::Linux, InstallKind::AppImage) => "AppImage",
        (Platform::Linux, InstallKind::Deb) => "deb",
        (Platform::Linux, InstallKind::Rpm) => "rpm",
        (Platform::Linux, InstallKind::Flatpak) => "flatpak",
        (Platform::Freebsd, InstallKind::Unknown | InstallKind::Development) => return PackageSelection::Unknown,
        _ => return PackageSelection::Unknown,
    };
    let expected = if extension.starts_with('-') {
        format!("pdfcraft-{version}-{platform}-{suffix}{extension}")
    } else {
        format!("pdfcraft-{version}-{platform}-{suffix}.{extension}")
    };
    let mut found = release.assets.iter().filter(|asset| asset.name == expected && validate_asset_url(&asset.url, tag, &asset.name).is_ok());
    match (found.next(), found.next()) {
        (Some(asset), None) => PackageSelection::Package(asset),
        (None, _) => PackageSelection::Missing,
        (Some(_), Some(_)) => PackageSelection::Ambiguous,
    }
}

fn parse_version(value: &str) -> Option<Version> {
    let value = value.trim();
    Version::parse(value.strip_prefix(['v', 'V']).unwrap_or(value)).ok()
}

/// Checks that a release page is the exact canonical GitHub page for `tag`.
pub fn valid_release_url(value: &str, tag: &str) -> bool {
    validate_release_url(value, tag).is_ok()
}

fn validate_release_url(value: &str, tag: &str) -> Result<(), String> {
    let normalized = tag.strip_prefix(['v', 'V']).unwrap_or(tag);
    if parse_version(normalized).is_none() {
        return Err("release tag is invalid".into());
    }
    let parsed = Url::parse(value).map_err(|_| "release URL is invalid")?;
    let expected_path = format!("/storytold/pdfcraft/releases/tag/{tag}");
    if parsed.scheme() != "https"
        || parsed.host_str() != Some("github.com")
        || parsed.port().is_some()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || parsed.path() != expected_path
        || value.contains(['\\', '%'])
    {
        return Err("release URL is not the canonical PdfCraft release page".into());
    }
    Ok(())
}

fn validate_asset_url(value: &str, tag: &str, name: &str) -> Result<(), String> {
    let parsed = Url::parse(value).map_err(|_| "asset download URL is invalid")?;
    let expected_path = format!("/storytold/pdfcraft/releases/download/{tag}/{name}");
    if parsed.scheme() != "https"
        || parsed.host_str() != Some("github.com")
        || parsed.port().is_some()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || parsed.path() != expected_path
        || value.contains(['\\', '%'])
    {
        return Err("asset URL is not the canonical PdfCraft download URL".into());
    }
    Ok(())
}

fn safe_digest(value: &str) -> bool {
    let Some((algorithm, hex)) = value.split_once(':') else {
        return false;
    };
    let expected_len = match algorithm {
        "sha256" => 64,
        "sha512" => 128,
        _ => return false,
    };
    hex.len() == expected_len && hex.bytes().all(|b| b.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response(names: &[&str]) -> String {
        response_for("v0.7.1", names)
    }

    fn response_for(tag: &str, names: &[&str]) -> String {
        let assets = names
            .iter()
            .map(|name| format!(r#"{{"name":"{name}","size":23,"browser_download_url":"https://github.com/storytold/pdfcraft/releases/download/{tag}/{name}","digest":"sha256:{}"}}"#, "a".repeat(64)))
            .collect::<Vec<_>>()
            .join(",");
        format!(
            r#"{{"tag_name":"{tag}","html_url":"https://github.com/storytold/pdfcraft/releases/tag/{tag}","body":"Notes","draft":false,"prerelease":false,"assets":[{assets}]}}"#
        )
    }

    #[test]
    fn parses_release_and_matches_published_package_names() {
        let release = parse_release(&response(&[
            "pdfcraft-0.7.1-windows-x64.msi",
            "pdfcraft-0.7.1-windows-x64-portable.zip",
            "pdfcraft-0.7.1-windows-x86.msi",
            "pdfcraft-0.7.1-windows-x86-portable.zip",
            "pdfcraft-0.7.1-windows-arm64.msi",
            "pdfcraft-0.7.1-windows-arm64-portable.zip",
            "pdfcraft-0.7.1-macos-universal.dmg",
            "pdfcraft-0.7.1-linux-x86_64.AppImage",
            "pdfcraft-0.7.1-linux-x86_64.deb",
            "pdfcraft-0.7.1-linux-x86_64.rpm",
            "pdfcraft-0.7.1-linux-x86_64.flatpak",
            "pdfcraft-0.7.1-freebsd-x86_64.tar.gz",
        ]))
        .unwrap();
        let context = |platform, architecture, kind| InstallContext { platform, architecture, kind };
        for (ctx, filename) in [
            (context(Platform::Windows, Architecture::X64, InstallKind::WindowsMsi), "pdfcraft-0.7.1-windows-x64.msi"),
            (context(Platform::Windows, Architecture::X64, InstallKind::WindowsPortable), "pdfcraft-0.7.1-windows-x64-portable.zip"),
            (context(Platform::Windows, Architecture::X86, InstallKind::WindowsMsi), "pdfcraft-0.7.1-windows-x86.msi"),
            (context(Platform::Windows, Architecture::X86, InstallKind::WindowsPortable), "pdfcraft-0.7.1-windows-x86-portable.zip"),
            (context(Platform::Windows, Architecture::Arm64, InstallKind::WindowsMsi), "pdfcraft-0.7.1-windows-arm64.msi"),
            (context(Platform::Windows, Architecture::Arm64, InstallKind::WindowsPortable), "pdfcraft-0.7.1-windows-arm64-portable.zip"),
            (context(Platform::Macos, Architecture::X64, InstallKind::MacApp), "pdfcraft-0.7.1-macos-universal.dmg"),
            (context(Platform::Linux, Architecture::X64, InstallKind::AppImage), "pdfcraft-0.7.1-linux-x86_64.AppImage"),
            (context(Platform::Linux, Architecture::X64, InstallKind::Deb), "pdfcraft-0.7.1-linux-x86_64.deb"),
            (context(Platform::Linux, Architecture::X64, InstallKind::Rpm), "pdfcraft-0.7.1-linux-x86_64.rpm"),
            (context(Platform::Linux, Architecture::X64, InstallKind::Flatpak), "pdfcraft-0.7.1-linux-x86_64.flatpak"),
            (context(Platform::Freebsd, Architecture::X64, InstallKind::Unknown), "pdfcraft-0.7.1-freebsd-x86_64.tar.gz"),
        ] {
            if filename.ends_with(".flatpak") || filename.ends_with(".deb") || filename.ends_with(".rpm") {
                assert_eq!(select_package(&release, ctx), PackageSelection::Managed(ctx.kind));
            } else if filename.ends_with(".tar.gz") {
                assert_eq!(select_package(&release, ctx), PackageSelection::Unknown);
            } else {
                assert!(matches!(select_package(&release, ctx), PackageSelection::Package(a) if a.name == filename));
            }
        }
        assert_eq!(release.notes, "Notes");
        assert_eq!(release.assets[0].digest.as_deref(), Some(format!("sha256:{}", "a".repeat(64)).as_str()));
    }

    #[test]
    fn semver_precedence_and_invalid_versions() {
        assert!(is_newer("v1.2.3", "1.2.2"));
        assert!(is_newer("1.2.3", "1.2.3-alpha.2"));
        assert!(!is_newer("1.2.3-alpha.1", "1.2.3-alpha.2"));
        assert!(is_newer("0.8.0-rc.10", "0.8.0-rc.2"));
        assert!(!is_newer("0.8.0-rc.2", "0.8.0-rc.10"));
        assert!(!is_newer("1.2.3+one", "1.2.3+two"));
        assert!(!is_newer("1.2", "1.0.0"));
    }

    #[test]
    fn invalid_and_unbounded_metadata_is_rejected() {
        let valid = response(&[]);
        assert!(parse_release(&format!("{} ", " ".repeat(MAX_BODY))).is_err());
        assert!(parse_release(&valid.replace("\"draft\":false", "\"draft\":true")).unwrap().draft);
        assert!(parse_release(&valid.replace("\"prerelease\":false", "\"prerelease\":true")).unwrap().prerelease);
        let rc = response_for("v0.8.0-rc.2", &[]);
        assert!(parse_release(&rc).is_err(), "a SemVer prerelease tag cannot claim stable status");
        assert!(parse_release(&rc.replace("\"prerelease\":false", "\"prerelease\":true")).unwrap().prerelease);
        assert_eq!(parse_release(&valid.replace("\"body\":\"Notes\"", "\"body\":null")).unwrap().notes, "");
        assert!(parse_release(&valid.replace(",\"body\":\"Notes\"", "")).is_err());
        assert!(parse_release(&valid.replace("\"body\":\"Notes\"", &format!("\"body\":\"{}\"", "x".repeat(MAX_NOTES + 1)))).is_err());
        assert!(parse_release(&valid.replace("https://github.com/storytold/pdfcraft/releases/tag/v0.7.1", "https://evil.example/")).is_err());
        assert!(parse_release(&response(&["pdfcraft-0.7.1-linux-x86_64.deb", "pdfcraft-0.7.1-linux-x86_64.deb"])).is_err());
        assert!(parse_release(&valid.replace("\"tag_name\":\"v0.7.1\"", "\"tag_name\":\"v0.7.1\",\"tag_name\":\"v0.7.1\"")).is_err());
        let with_asset = response(&["sample.zip"]);
        assert!(parse_release(&with_asset.replace("\"name\":\"sample.zip\"", "\"name\":\"sample.zip\",\"name\":\"sample.zip\"")).is_err());
        assert!(parse_release("<html>").is_err());
    }

    #[test]
    fn asset_fields_and_each_metadata_limit_are_enforced() {
        let valid = response(&["sample.zip"]);
        for malformed in [
            valid.replace("\"name\":\"sample.zip\",", ""),
            valid.replace(",\"size\":23", ""),
            valid.replace(",\"browser_download_url\":\"https://github.com/storytold/pdfcraft/releases/download/v0.7.1/sample.zip\"", ""),
        ] {
            assert!(parse_release(&malformed).is_err());
        }

        let oversized_name = "n".repeat(MAX_ASSET_NAME + 1);
        assert!(parse_release(&response(&[&oversized_name])).is_err());
        let too_many: Vec<String> = (0..=MAX_ASSETS).map(|i| format!("file-{i}.zip")).collect();
        let too_many_refs: Vec<&str> = too_many.iter().map(String::as_str).collect();
        assert!(parse_release(&response(&too_many_refs)).is_err());

        let too_long_url = format!("https://github.com/storytold/pdfcraft/releases/download/v0.7.1/{}", "x".repeat(MAX_URL));
        assert!(parse_release(&valid.replace("https://github.com/storytold/pdfcraft/releases/download/v0.7.1/sample.zip", &too_long_url)).is_err());
        let oversized_digest = format!("sha512:{}", "a".repeat(134));
        let digest_needle = format!("sha256:{}", "a".repeat(64));
        assert!(parse_release(&valid.replace(&digest_needle, &oversized_digest)).is_err());
        assert!(parse_release(&valid.replace(&digest_needle, "sha256:xyz")).is_err());
    }

    #[test]
    fn selection_covers_linux_and_macos_architectures_and_incompatible_contexts() {
        let linux = parse_release(&response(&["pdfcraft-0.7.1-linux-aarch64.AppImage"])).unwrap();
        let arm = InstallContext { platform: Platform::Linux, architecture: Architecture::Arm64, kind: InstallKind::AppImage };
        assert!(matches!(select_package(&linux, arm), PackageSelection::Package(asset) if asset.name == "pdfcraft-0.7.1-linux-aarch64.AppImage"));
        let riscv = InstallContext { architecture: Architecture::Riscv64, ..arm };
        assert_eq!(select_package(&linux, riscv), PackageSelection::Missing);
        let unsupported = InstallContext { platform: Platform::Unsupported, architecture: Architecture::X64, kind: InstallKind::AppImage };
        assert_eq!(select_package(&linux, unsupported), PackageSelection::Unknown);
        let incompatible = InstallContext { platform: Platform::Macos, architecture: Architecture::X64, kind: InstallKind::WindowsMsi };
        assert_eq!(select_package(&linux, incompatible), PackageSelection::Unknown);

        let mac = parse_release(&response(&["pdfcraft-0.7.1-macos-universal.dmg"])).unwrap();
        for architecture in [Architecture::X64, Architecture::Arm64] {
            let context = InstallContext { platform: Platform::Macos, architecture, kind: InstallKind::MacApp };
            assert!(matches!(select_package(&mac, context), PackageSelection::Package(asset) if asset.name == "pdfcraft-0.7.1-macos-universal.dmg"));
        }
    }

    #[test]
    fn ambiguous_manual_asset_lists_are_not_resolved_arbitrarily() {
        let mut release = parse_release(&response(&["pdfcraft-0.7.1-windows-x64.msi"])).unwrap();
        let duplicate = release.assets[0].clone();
        release.assets.push(duplicate);
        let context = InstallContext { platform: Platform::Windows, architecture: Architecture::X64, kind: InstallKind::WindowsMsi };
        assert_eq!(select_package(&release, context), PackageSelection::Ambiguous);
    }

    #[test]
    fn unknown_managed_and_missing_package_outcomes_are_distinct() {
        let release = parse_release(&response(&[])).unwrap();
        let context = InstallContext { platform: Platform::Linux, architecture: Architecture::Unknown, kind: InstallKind::AppImage };
        assert_eq!(select_package(&release, context), PackageSelection::Unknown);
        let managed = InstallContext { platform: Platform::Linux, architecture: Architecture::X64, kind: InstallKind::Flatpak };
        assert_eq!(select_package(&release, managed), PackageSelection::Managed(InstallKind::Flatpak));
        let missing = InstallContext { platform: Platform::Linux, architecture: Architecture::X64, kind: InstallKind::Deb };
        assert_eq!(select_package(&release, missing), PackageSelection::Managed(InstallKind::Deb));
        let managed = InstallContext { kind: InstallKind::Rpm, ..missing };
        assert_eq!(select_package(&release, managed), PackageSelection::Managed(InstallKind::Rpm));
        assert_eq!(InstallContext::default().kind, InstallKind::Unknown);
    }

    #[test]
    fn manually_constructed_release_cannot_inject_download_urls() {
        let mut release = parse_release(&response(&["pdfcraft-0.7.1-windows-x64.msi"])).unwrap();
        release.assets[0].url = "https://evil.example/payload.deb".into();
        let context = InstallContext { platform: Platform::Windows, architecture: Architecture::X64, kind: InstallKind::WindowsMsi };
        assert_eq!(select_package(&release, context), PackageSelection::Missing);
        assert!(!valid_release_url("https://github.com.evil/storytold/pdfcraft/releases/tag/v0.7.1", "v0.7.1"));
    }
    #[test]
    fn duplicate_version_prefixes_and_contradictory_contexts_are_rejected() {
        assert!(parse_release(&response(&[]).replace("v0.7.1", "vv0.7.1")).is_err());
        assert!(!is_newer("vv1.2.3", "1.0.0"));
        let release = parse_release(&response(&[])).unwrap();
        for kind in [InstallKind::Deb, InstallKind::Rpm, InstallKind::Flatpak] {
            let context = InstallContext { platform: Platform::Windows, architecture: Architecture::X64, kind };
            assert_eq!(select_package(&release, context), PackageSelection::Unknown);
        }
    }
}
