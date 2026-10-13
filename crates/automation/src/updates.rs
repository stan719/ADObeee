//! Headless release-metadata evaluation; this module never performs network or file I/O.

use pdfcraft_platform::updates::{Architecture, InstallContext, InstallKind, PackageSelection, Platform, is_newer, parse_release, select_package};
use serde_json::{Value, json};

use crate::{Args, Result, ToolError};

pub(super) fn update_release_info(args: &Args<'_>) -> Result<Value> {
    let metadata = args.str("metadata_json")?;
    let release = parse_release(metadata).map_err(ToolError::InvalidArgs)?;
    let installed_version = args.str("installed_version")?;
    if installed_version.len() > 128 {
        return Err(ToolError::InvalidArgs("installed_version is too long".into()));
    }
    let platform_name = bounded_context(args, "platform")?;
    let platform = match platform_name {
        "windows" => Platform::Windows,
        "macos" => Platform::Macos,
        "linux" => Platform::Linux,
        "freebsd" => Platform::Freebsd,
        "unsupported" => Platform::Unsupported,
        other => return Err(ToolError::InvalidArgs(format!("unknown platform {other:?}"))),
    };
    let architecture_name = bounded_context(args, "architecture")?;
    let architecture = match architecture_name {
        "x86" => Architecture::X86,
        "x64" => Architecture::X64,
        "arm64" => Architecture::Arm64,
        "riscv64" => Architecture::Riscv64,
        "unknown" => Architecture::Unknown,
        other => return Err(ToolError::InvalidArgs(format!("unknown architecture {other:?}"))),
    };
    let installation_name = bounded_context(args, "installation")?;
    let kind = match installation_name {
        "windows_msi" => InstallKind::WindowsMsi,
        "windows_portable" => InstallKind::WindowsPortable,
        "mac_app" => InstallKind::MacApp,
        "appimage" => InstallKind::AppImage,
        "deb" => InstallKind::Deb,
        "rpm" => InstallKind::Rpm,
        "flatpak" => InstallKind::Flatpak,
        "unknown" => InstallKind::Unknown,
        "development" => InstallKind::Development,
        other => return Err(ToolError::InvalidArgs(format!("unknown installation {other:?}"))),
    };
    let context = InstallContext { platform, architecture, kind };
    let non_stable = release.draft || release.prerelease;
    let (selection, package) = if non_stable {
        ("non_stable", Value::Null)
    } else {
        match select_package(&release, context) {
            PackageSelection::Package(asset) => {
                ("package", json!({ "name": asset.name, "size": asset.size, "url": asset.url, "digest": asset.digest }))
            }
            PackageSelection::Managed(_) => ("managed", Value::Null),
            PackageSelection::Unknown => ("unknown", Value::Null),
            PackageSelection::Missing => ("missing", Value::Null),
            PackageSelection::Ambiguous => ("ambiguous", Value::Null),
        }
    };
    Ok(json!({
        "version": release.version,
        "url": release.url,
        "notes": release.notes,
        "draft": release.draft,
        "prerelease": release.prerelease,
        "newer": !non_stable && is_newer(&release.version, installed_version),
        "package": package,
        "selection": selection,
    }))
}

fn bounded_context<'a>(args: &'a Args<'_>, field: &str) -> Result<&'a str> {
    let value = args.str(field)?;
    if value.len() > 32 {
        return Err(ToolError::InvalidArgs(format!("{field} is too long")));
    }
    Ok(value)
}
