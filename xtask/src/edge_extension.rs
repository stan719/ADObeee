//! `cargo xtask edge-extension`: package the web build as an unpacked Edge extension.
//!
//! The web build's startup module and styles are generated inline by Trunk. Extension pages cannot
//! run inline scripts under Manifest V3, so this task moves the generated module and styles into
//! external files. The JavaScript comes from Trunk's output; this module adds no handwritten JS.

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

const START: &str = "<script type=\"module\">";
const END: &str = "</script>";
const EXTERNAL_BOOT: &str = "<script type=\"module\" src=\"./extension-boot.js\"></script>";
const STYLE_START: &str = "<style>";
const STYLE_END: &str = "</style>";
const EXTERNAL_STYLE: &str = "<link rel=\"stylesheet\" href=\"./extension.css\">";
const MAX_SITE_DEPTH: usize = 64;

/// `cargo xtask edge-extension [--site DIR] [--out DIR]`.
pub fn run(args: &[String]) -> Result<()> {
    let root = crate::gates::root();
    let (site_arg, out_arg) = parse_args(args)?;
    let site_path = site_arg.as_deref().map_or(Path::new("dist/web"), |path| path);
    let site = absolute_from(&root, site_path).canonicalize().with_context(|| format!("web site does not exist: {}", site_path.display()))?;
    let index = site.join("index.html");
    let html = fs::read_to_string(&index).with_context(|| format!("read {}", index.display()))?;
    let (without_startup, boot_js) = extract_inline(&html, START, END, EXTERNAL_BOOT, "module startup script")?;
    let (extension_html, extension_css) = extract_inline(&without_startup, STYLE_START, STYLE_END, EXTERNAL_STYLE, "inline style block")?;

    let out_arg = out_arg.unwrap_or_else(|| PathBuf::from("target/edge-extension"));
    let out = absolute_from(&root, &out_arg);
    fs::create_dir_all(&out).with_context(|| format!("create {}", out.display()))?;
    let out = out.canonicalize().with_context(|| format!("resolve {}", out.display()))?;
    if out.starts_with(&site) || site.starts_with(&out) {
        bail!("output and web site must be separate directories");
    }

    copy_site(&site, &out, 0)?;
    fs::write(out.join("index.html"), extension_html).context("write extension index.html")?;
    fs::write(out.join("extension-boot.js"), boot_js).context("write generated extension bootstrap")?;
    fs::write(out.join("extension.css"), extension_css).context("write generated extension styles")?;
    write_manifest(&root, &out)?;
    write_icons(&root, &out)?;
    fs::write(out.join("open.html"), OPEN_PAGE).context("write extension launcher")?;
    let instructions = root.join("packaging/edge-extension/README.md");
    fs::copy(&instructions, out.join("README-Install.md")).with_context(|| format!("copy {}", instructions.display()))?;

    println!("wrote unpacked Edge extension to {}", out.display());
    Ok(())
}

fn parse_args(args: &[String]) -> Result<(Option<PathBuf>, Option<PathBuf>)> {
    let mut site = None;
    let mut out = None;
    let mut i = 0usize;
    while i < args.len() {
        let name = args.get(i).context("missing argument")?;
        match name.as_str() {
            "--site" => {
                if site.is_some() {
                    bail!("{name} may be specified once");
                }
                let value_index = i.checked_add(1).context("argument index overflow")?;
                let value = args.get(value_index).with_context(|| format!("{name} needs a directory"))?;
                site = Some(PathBuf::from(value));
            }
            "--out" => {
                if out.is_some() {
                    bail!("{name} may be specified once");
                }
                let value_index = i.checked_add(1).context("argument index overflow")?;
                let value = args.get(value_index).with_context(|| format!("{name} needs a directory"))?;
                out = Some(PathBuf::from(value));
            }
            _ => bail!("unknown argument `{name}`; usage: cargo xtask edge-extension [--site DIR] [--out DIR]"),
        }
        i = i.checked_add(2).context("argument index overflow")?;
    }
    Ok((site, out))
}

fn absolute_from(root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() { path.to_path_buf() } else { root.join(path) }
}

fn extract_inline(html: &str, start: &str, end: &str, external: &str, description: &str) -> Result<(String, String)> {
    let mut starts = html.match_indices(start);
    let (start_at, _) = starts.next().with_context(|| format!("Trunk index.html has no {description}"))?;
    if starts.next().is_some() {
        bail!("Trunk index.html has more than one {description}; update edge-extension packaging");
    }
    let body_at = start_at.checked_add(start.len()).context("inline block offset overflow")?;
    let relative_end =
        html.get(body_at..).context("invalid inline block boundary")?.find(end).with_context(|| format!("Trunk {description} is not closed"))?;
    let end_at = body_at.checked_add(relative_end).context("inline block offset overflow")?;
    let tag_end = end_at.checked_add(end.len()).context("inline block offset overflow")?;
    let body = html.get(body_at..end_at).context("invalid inline block body")?.to_string();
    let mut rewritten = String::with_capacity(html.len());
    rewritten.push_str(html.get(..start_at).context("invalid inline block prefix")?);
    rewritten.push_str(external);
    rewritten.push_str(html.get(tag_end..).context("invalid inline block suffix")?);
    Ok((rewritten, body))
}

fn copy_site(source: &Path, destination: &Path, depth: usize) -> Result<()> {
    if depth > MAX_SITE_DEPTH {
        bail!("web site directory nesting exceeds {MAX_SITE_DEPTH} levels");
    }
    for entry in fs::read_dir(source).with_context(|| format!("read site directory {}", source.display()))? {
        let entry = entry.context("read site entry")?;
        let name = entry.file_name();
        if excluded_site_entry(&name) {
            continue;
        }
        if name.to_string_lossy().starts_with('_') {
            bail!("Edge reserves underscore-prefixed extension paths: {}", entry.path().display());
        }
        let source_path = entry.path();
        let target_path = destination.join(&name);
        let kind = entry.file_type().with_context(|| format!("inspect {}", source_path.display()))?;
        if kind.is_symlink() {
            bail!("web site contains a symbolic link: {}", source_path.display());
        } else if kind.is_dir() {
            fs::create_dir_all(&target_path).with_context(|| format!("create {}", target_path.display()))?;
            let child_depth = depth.checked_add(1).context("web site nesting depth overflow")?;
            copy_site(&source_path, &target_path, child_depth)?;
        } else if kind.is_file() {
            fs::copy(&source_path, &target_path).with_context(|| format!("copy {}", source_path.display()))?;
        } else {
            bail!("web site contains an unsupported file: {}", source_path.display());
        }
    }
    Ok(())
}

fn excluded_site_entry(name: &OsStr) -> bool {
    let name = name.to_string_lossy();
    name == "_headers" || name == "_redirects" || name == "_routes.json" || name == ".htaccess" || name == "HOSTING.md"
}

fn write_manifest(root: &Path, out: &Path) -> Result<()> {
    let cargo_toml = fs::read_to_string(root.join("Cargo.toml")).context("read workspace Cargo.toml")?;
    let manifest: toml::Value = toml::from_str(&cargo_toml).context("parse workspace Cargo.toml")?;
    let version = manifest
        .get("workspace")
        .and_then(|v| v.get("package"))
        .and_then(|v| v.get("version"))
        .and_then(toml::Value::as_str)
        .context("workspace.package.version is missing")?;
    let extension_version = extension_version(version)?;
    let value = serde_json::json!({
        "manifest_version": 3,
        "name": "PdfCraft",
        "version": extension_version,
        "version_name": version,
        "description": "Open PdfCraft in a browser tab to work on local PDFs.",
        "icons": {
            "16": "icons/16.png",
            "32": "icons/32.png",
            "48": "icons/48.png",
            "128": "icons/128.png"
        },
        "action": {
            "default_title": "Open PdfCraft",
            "default_popup": "open.html",
            "default_icon": {
                "16": "icons/16.png",
                "32": "icons/32.png",
                "48": "icons/48.png",
                "128": "icons/128.png"
            }
        },
        "content_security_policy": {
            "extension_pages": "script-src 'self' 'wasm-unsafe-eval'; style-src 'self'; object-src 'self'"
        }
    });
    let bytes = serde_json::to_vec_pretty(&value).context("serialize extension manifest")?;
    fs::write(out.join("manifest.json"), bytes).context("write extension manifest")
}

fn extension_version(version: &str) -> Result<String> {
    let (release, pre) = version.split_once('-').map_or((version, None), |(release, pre)| (release, Some(pre)));
    let parts: Vec<&str> = release.split('.').collect();
    if parts.len() != 3 {
        bail!("workspace version must have three numeric components: {version}");
    }
    let mut numbers = Vec::with_capacity(3);
    for part in parts {
        if part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()) {
            bail!("workspace version must have three numeric components: {version}");
        }
        numbers.push(part.parse::<u16>().with_context(|| format!("workspace version component is out of range: {version}"))?);
    }
    let fourth = match pre {
        None => 65_535,
        Some(label) => {
            let mut identifiers = label.split('.');
            let class = identifiers.next().context("empty prerelease version")?;
            if class.is_empty() {
                bail!("empty prerelease version: {version}");
            }
            let rank = match class {
                "alpha" | "a" => 0_u16,
                "beta" | "b" => 1,
                "rc" => 2,
                _ => 3,
            };
            let last_identifier = label.rsplit('.').next().context("empty prerelease version")?;
            let sequence = if last_identifier.bytes().all(|byte| byte.is_ascii_digit()) {
                last_identifier.parse::<u16>().with_context(|| format!("prerelease sequence is out of range: {version}"))?
            } else {
                0
            };
            if sequence > 16_382 {
                bail!("prerelease sequence is too large for an Edge extension version: {version}");
            }
            rank * 16_384 + sequence
        }
    };
    let major = numbers.first().context("missing major version")?;
    let minor = numbers.get(1).context("missing minor version")?;
    let patch = numbers.get(2).context("missing patch version")?;
    Ok(format!("{major}.{minor}.{patch}.{fourth}"))
}

fn write_icons(root: &Path, out: &Path) -> Result<()> {
    let icons = out.join("icons");
    fs::create_dir_all(&icons).context("create extension icons directory")?;
    for size in [16, 32, 48, 128] {
        let source = root.join(format!("assets/app-icon/hicolor/{size}x{size}/apps/ai.storyteller.pdfcraft.png"));
        fs::copy(&source, icons.join(format!("{size}.png"))).with_context(|| format!("copy attributed app icon {}", source.display()))?;
    }
    Ok(())
}

const OPEN_PAGE: &str = r#"<!doctype html>
<html lang="en">
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Open PdfCraft</title>
<h1>PdfCraft</h1>
<p>Open the PDF workbench in a browser tab.</p>
<p><a href="index.html" target="_blank">Open PdfCraft</a></p>
</html>
"#;

#[cfg(test)]
mod tests {
    use super::{excluded_site_entry, extension_version, extract_inline};
    use std::ffi::OsStr;

    #[test]
    fn extracts_one_inline_block_and_rewrites_its_tag() {
        let (html, body) = extract_inline(
            "before<script type=\"module\">boot()</script>after",
            "<script type=\"module\">",
            "</script>",
            "<script src=\"./extension-boot.js\"></script>",
            "module startup script",
        )
        .unwrap();
        assert_eq!(body, "boot()");
        assert_eq!(html, "before<script src=\"./extension-boot.js\"></script>after");
    }

    #[test]
    fn rejects_missing_or_duplicate_inline_blocks() {
        assert!(extract_inline("plain", "<style>", "</style>", "link", "style block").is_err());
        assert!(extract_inline("<style>a</style><style>b</style>", "<style>", "</style>", "link", "style block",).is_err());
    }

    #[test]
    fn maps_semver_to_edge_extension_version() {
        assert_eq!(extension_version("0.7.1").unwrap(), "0.7.1.65535");
        assert_eq!(extension_version("1.2.3-alpha.4").unwrap(), "1.2.3.4");
        assert_eq!(extension_version("1.2.3-beta.2").unwrap(), "1.2.3.16386");
        assert_eq!(extension_version("1.2.3-rc.1").unwrap(), "1.2.3.32769");
        assert!(extension_version("1.2").is_err());
    }

    #[test]
    fn excludes_hosting_files_and_keeps_normal_files() {
        assert!(excluded_site_entry(OsStr::new("_headers")));
        assert!(excluded_site_entry(OsStr::new(".htaccess")));
        assert!(!excluded_site_entry(OsStr::new("index.html")));
    }
}
