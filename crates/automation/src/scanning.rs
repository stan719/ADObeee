//! Scanner listing and acquire. Listing does not feed paper.

use std::path::Path;

use pdfcraft_platform::scan;
use serde_json::{Value, json};

use crate::{Args, Automation, Result, ToolError, failed};

impl Automation {
    pub(crate) fn scan_devices(&self, args: &Args) -> Result<Value> {
        list_scan_devices(args.opt_str("device")?, args.opt_bool("details")?.unwrap_or(false))
    }

    pub(crate) fn scan_acquire(&self, args: &Args) -> Result<Value> {
        let out = args.str("out")?;
        let folder = self.resolve(out, true)?;
        std::fs::create_dir_all(&folder).map_err(|error| failed(format!("{out}: {error}")))?;
        let settings = scan_settings_from_args(args)?;
        settings.checked().map_err(|error| failed(error.to_string()))?;
        match scan::acquire(&settings) {
            Ok(acquired) => finish_acquire_json(&folder, &settings, &acquired, false, None),
            Err(error) => acquire_failure(&folder, &settings, error),
        }
    }
}

/// `{kind, phase, hresult, code_name, retryable, resumable, message}` for a failed scan: the
/// identity the engine settled on, so a caller reads fields and never the sentence. `hresult` is
/// hex text (`0x80210006`); absent values are `null`. The vocabulary is `pdfcraft_scan`'s.
pub fn scan_error_info_json(info: &scan::ScanErrorInfo) -> Value {
    json!({
        "kind": info.kind.as_str(),
        "phase": info.phase.as_str(),
        "hresult": info.hresult_hex(),
        "code_name": info.code_name,
        "retryable": info.retryable,
        "resumable": info.resumable,
        "message": info.message,
    })
}

/// A failed acquire: pages that arrived are saved and reported as `partial` with `error` and
/// `error_info`; with none, the call fails and the failure's data rides along.
fn acquire_failure(folder: &Path, settings: &scan::ScanSettings, error: scan::ScanError) -> Result<Value> {
    let info = scan::ScanErrorInfo::from_acquire(&error, settings);
    match error {
        scan::ScanError::Incomplete { message, acquired, .. } => {
            let mut out = finish_acquire_json(folder, settings, &acquired, true, Some(message))?;
            out["error_info"] = scan_error_info_json(&info);
            Ok(out)
        }
        other => {
            let message = match other {
                scan::ScanError::Unsupported => scan::ScanError::Unsupported.to_string(),
                scan::ScanError::Failed(message) | scan::ScanError::Wia { message, .. } => message,
                scan::ScanError::Incomplete { message, .. } => message,
            };
            Err(ToolError::FailedWith { message, data: scan_error_info_json(&info) })
        }
    }
}

fn scan_settings_from_args(args: &Args) -> Result<scan::ScanSettings> {
    let pages = if let Some(n) = args.opt_int("pages")? {
        u32::try_from(n).map_err(|_| ToolError::InvalidArgs("pages must be non-negative".into()))?
    } else if let Some(text) = args.opt_str("pages")? {
        if text == "all" { 0 } else { text.parse::<u32>().map_err(|_| ToolError::InvalidArgs("pages must be a whole number or all".into()))? }
    } else {
        1
    };
    let color_text = args.opt_str("color")?.unwrap_or("gray");
    let color = scan::Color::parse(color_text).ok_or_else(|| ToolError::InvalidArgs(format!("color must be gray, rgb, or bw (got {color_text})")))?;
    let source = scan::Source::parse(args.opt_str("source")?.unwrap_or("auto"))
        .ok_or_else(|| ToolError::InvalidArgs("source must be auto, feeder, or glass".into()))?;
    let dpi = args.opt_int("dpi")?.unwrap_or(200);
    if dpi < 1 {
        return Err(ToolError::InvalidArgs("dpi must be positive".into()));
    }
    Ok(scan::ScanSettings {
        device: args.opt_str("device")?.unwrap_or("").to_string(),
        source,
        dpi: u32::try_from(dpi).map_err(|_| ToolError::InvalidArgs("dpi out of range".into()))?,
        dpi_y: args.opt_int("dpi_y")?.map(|n| u32::try_from(n).map_err(|_| ToolError::InvalidArgs("dpi_y out of range".into()))).transpose()?,
        color,
        pages,
        brightness: None,
        contrast: None,
        threshold: None,
        rotation: None,
        region: None,
        format: args.opt_str("format")?.map(str::to_string),
        compression: None,
        intent: None,
        page_size: None,
        auto_deskew: None,
        orientation: None,
        photometric: None,
        invert: None,
        mirror: None,
        lamp: None,
        duplex: args.opt_bool("duplex")?.unwrap_or(false),
        cancel: None,
        progress: None,
    })
}

fn finish_acquire_json(
    folder: &Path,
    settings: &scan::ScanSettings,
    acquired: &scan::Acquired,
    partial: bool,
    error: Option<String>,
) -> Result<Value> {
    let paths = scan::write_pages(folder, &acquired.pages).map_err(failed)?;
    let pages: Vec<Value> = paths
        .iter()
        .zip(acquired.pages.iter())
        .map(|(path, page)| {
            json!({
                "path": path.display().to_string(),
                "format": page.format,
                "width": page.width,
                "height": page.height,
                "dpi": page.dpi,
                "bytes": page.bytes.len(),
                "thresholded": page.thresholded,
            })
        })
        .collect();
    let mut out = json!({
        "source": acquired.source.as_str(),
        "requested_source": settings.source.as_str(),
        "reverse": false,
        "partial": partial,
        "count": pages.len(),
        "pages": pages,
    });
    if let Some(message) = error {
        out["error"] = json!(message);
    }
    Ok(out)
}

/// Scanner listing.
///
/// By default nothing is opened: each row is `{id, name, port, connection, details: "skipped"}`.
/// `filter` (case-insensitive id, name or port substring) opens only the matching devices and
/// returns only those, with `details: "loaded"` and the full capability keys. `details == true`
/// without a filter opens every device that is not denied. A device that fails to open stays in
/// the list as `details: "error"` with the engine's own `error` sentence. A denied device (see
/// `PDFCRAFT_SCAN_DENY`) is never opened: it is listed as `details: "denied"`, and a filter that
/// names or selects one is refused.
pub fn list_scan_devices(filter: Option<&str>, details: bool) -> Result<Value> {
    let denied = scan::deny_list();
    check_filter(filter, &denied)?;
    let listed = scan::scanner_list().map_err(failed)?;
    build_listing(filter, details, &denied, &listed, &mut |id| scan::scanner_details(id))
}

/// One device by exact WIA id, as a `details` row (`loaded`, or `error` when it will not open).
/// Opens that device only. A denied or unknown id is an error.
pub fn scan_device_details(id: &str) -> Result<Value> {
    let denied = scan::deny_list();
    check_filter(Some(id), &denied)?;
    let listed = scan::scanner_list().map_err(failed)?;
    detail_row_for(id, &denied, &listed, &mut |id| scan::scanner_details(id))
}

type OpenScanner<'a> = dyn FnMut(&str) -> std::result::Result<scan::Scanner, scan::ScanError> + 'a;

fn check_filter(filter: Option<&str>, denied: &[String]) -> Result<()> {
    let Some(text) = filter else {
        return Ok(());
    };
    if text.trim().is_empty() {
        return Err(ToolError::InvalidArgs("the device filter must not be empty".into()));
    }
    if scan::filter_is_denied(text, denied) {
        return Err(denied_error());
    }
    Ok(())
}

fn denied_error() -> ToolError {
    ToolError::InvalidArgs("refusing to list or open a denied scanner (PDFCRAFT_SCAN_DENY)".into())
}

fn build_listing(filter: Option<&str>, details: bool, denied: &[String], listed: &[scan::ScannerRef], open: &mut OpenScanner<'_>) -> Result<Value> {
    check_filter(filter, denied)?;
    let selected: Vec<&scan::ScannerRef> = listed.iter().filter(|item| filter.is_none_or(|text| item.matches(text))).collect();
    if filter.is_some() && selected.iter().any(|item| item.is_denied(denied)) {
        return Err(denied_error());
    }
    let open_them = details || filter.is_some();
    let rows: Vec<Value> = selected
        .into_iter()
        .map(|item| {
            if !open_them {
                ref_row(item, scan::ListDetails::Skipped)
            } else if item.is_denied(denied) {
                ref_row(item, scan::ListDetails::Denied)
            } else {
                opened_row(item, open)
            }
        })
        .collect();
    Ok(json!({ "count": rows.len(), "scanners": rows }))
}

fn detail_row_for(id: &str, denied: &[String], listed: &[scan::ScannerRef], open: &mut OpenScanner<'_>) -> Result<Value> {
    let item = listed.iter().find(|item| item.id == id).ok_or_else(|| failed(format!("no scanner with id {id:?}")))?;
    if item.is_denied(denied) {
        return Err(denied_error());
    }
    Ok(opened_row(item, open))
}

fn ref_row(item: &scan::ScannerRef, details: scan::ListDetails) -> Value {
    json!({ "id": item.id, "name": item.name, "port": item.port, "connection": item.connection.as_str(), "details": details.as_str() })
}

fn opened_row(item: &scan::ScannerRef, open: &mut OpenScanner<'_>) -> Value {
    match open(&item.id) {
        Ok(scanner) => {
            let mut row = scanner_json(&scanner);
            if let Some(map) = row.as_object_mut() {
                map.insert("details".into(), json!(scan::ListDetails::Loaded.as_str()));
            }
            row
        }
        Err(error) => {
            let mut row = ref_row(item, scan::ListDetails::Error);
            if let Some(map) = row.as_object_mut() {
                map.insert("error".into(), json!(error.to_string()));
            }
            row
        }
    }
}
fn scanner_json(scanner: &scan::Scanner) -> Value {
    let status = scanner.handling_status;
    json!({
        "id": scanner.id,
        "name": scanner.name,
        "feeder": scanner.feeder,
        "flatbed": scanner.flatbed,
        "duplex": scanner.duplex,
        "feeder_ready": scanner.feeder_ready,
        "handling_status": status_json(&status),
        "conditions": status.conditions(),
        "feeder_capacity": scanner.feeder_capacity,
        "connection": scanner.connection.as_str(),
        "port": scanner.port,
        "optical_x": scanner.optical_x,
        "optical_y": scanner.optical_y,
        "pages": span_json(scanner.pages),
        "bed": area_json(scanner.bed),
        "sheet": area_json(scanner.sheet),
        "glass": choices_json(scanner.glass.as_ref()),
        "feeder_choices": choices_json(scanner.feeder_choices.as_ref()),
    })
}

/// All nine handling flags, by the engine's own list of names.
fn status_json(status: &scan::HandlingStatus) -> Value {
    Value::Object(status.flags().into_iter().map(|(name, on)| (name.to_string(), Value::Bool(on))).collect())
}

fn choices_json(choices: Option<&scan::ImageChoices>) -> Value {
    let Some(choices) = choices else {
        return Value::Null;
    };
    json!({
        "dpi": choices.dpi,
        "dpi_y": choices.dpi_y,
        "colors": choices.colors.iter().map(|color| color.as_str()).collect::<Vec<_>>(),
        "brightness": span_json(choices.brightness),
        "contrast": span_json(choices.contrast),
        "threshold": span_json(choices.threshold),
        "rotation": choices.rotation.iter().map(|turn| turn.name()).collect::<Vec<_>>(),
        "formats": choices.formats,
        "compression": choices.compression,
        "intents": choices.intents.iter().map(|intent| intent.as_str()).collect::<Vec<_>>(),
        "page_sizes": choices.page_sizes.iter().map(|size| size.as_str()).collect::<Vec<_>>(),
        "auto_deskew": choices.auto_deskew,
        "orientation": choices.orientation,
        "photometric": choices.photometric,
        "invert": choices.invert,
        "mirror": choices.mirror,
        "lamp": choices.lamp,
    })
}

fn area_json(area: Option<(u32, u32)>) -> Value {
    area.map(|(width, height)| json!({ "width": width, "height": height })).unwrap_or(Value::Null)
}

fn span_json(span: Option<scan::ValueSpan>) -> Value {
    span.map(|span| json!({ "min": span.min, "max": span.max, "step": span.step })).unwrap_or(Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs on every platform: the unsupported answer (what non-Windows hosts give) is
    /// one clear sentence, as a tool error that carries the error data.
    #[test]
    fn the_unsupported_backend_reports_windows_only_for_listing_and_acquire() {
        let sentence = "scanning is only supported on Windows (WIA) in this version";
        let listed = build_listing(None, false, &[], &[], &mut |_| Err(scan::ScanError::Unsupported));
        assert!(listed.is_ok(), "an empty list is fine");
        assert_eq!(scan::ScanError::Unsupported.to_string(), sentence);
        let folder = std::env::temp_dir();
        let settings = scan::ScanSettings::default();
        let error = scan::ScanError::Unsupported;
        match acquire_failure(&folder, &settings, error) {
            Err(ToolError::FailedWith { message, .. }) => assert_eq!(message, sentence),
            other => panic!("expected a failed tool error, got {other:?}"),
        }
    }

    #[test]
    fn an_incomplete_scan_reports_partial_true_with_the_message_and_keeps_the_page() {
        let folder = std::env::temp_dir().join(format!("pdfcraft-scan-partial-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(&folder).unwrap();
        let page = scan::Page { bytes: b"\x89PNG\r\n\x1a\nxx".to_vec(), format: "png", width: 2, height: 2, dpi: 200, thresholded: false };
        let acquired = scan::Acquired { source: scan::Source::Feeder, pages: vec![page] };
        let settings = scan::ScanSettings { pages: 3, source: scan::Source::Feeder, ..scan::ScanSettings::default() };
        let message = "Only 1 of 3 pages were scanned: the feeder reported no more paper.".to_string();
        let json = finish_acquire_json(&folder, &settings, &acquired, true, Some(message.clone())).unwrap();
        assert_eq!(json["partial"], true, "{json}");
        assert_eq!(json["count"], 1);
        assert_eq!(json["error"], message.as_str());
        let text = json["error"].as_str().unwrap();
        assert!(text.contains("feeder") && text.contains("paper"), "{text}");
        assert_eq!(json["pages"].as_array().unwrap().len(), 1);
        assert!(std::path::Path::new(json["pages"][0]["path"].as_str().unwrap()).exists());
        let _ = std::fs::remove_dir_all(&folder);
    }

    fn item(id: &str, name: &str, port: Option<&str>) -> scan::ScannerRef {
        scan::ScannerRef::new(id.into(), name.into(), port.map(str::to_string))
    }

    fn fixture() -> Vec<scan::ScannerRef> {
        vec![
            item("{a}", "Fake Scanner", Some("urn:uuid:00000000-0000-4000-8000-0000000000f2/svc")),
            item("{b}", "Office Printer", Some("urn:uuid:99999999-aaaa-4bbb-8ccc-dddddddddddd/svc")),
            item("{c}", "Broken", None),
        ]
    }

    fn scanner_for(id: &str) -> scan::Scanner {
        scan::Scanner {
            id: id.into(),
            name: "x".into(),
            feeder: true,
            flatbed: true,
            duplex: false,
            feeder_ready: false,
            handling_status: scan::HandlingStatus::default(),
            feeder_capacity: None,
            connection: scan::Connection::Unknown,
            port: None,
            optical_x: None,
            optical_y: None,
            pages: None,
            bed: None,
            sheet: None,
            glass: None,
            feeder_choices: None,
        }
    }

    fn open_fake(opened: &mut Vec<String>, id: &str) -> std::result::Result<scan::Scanner, scan::ScanError> {
        opened.push(id.to_string());
        if id == "{c}" { Err(scan::ScanError::Failed("The device is busy.".into())) } else { Ok(scanner_for(id)) }
    }

    #[test]
    fn the_default_listing_is_cheap_and_opens_nothing() {
        let mut opened = Vec::new();
        let out = build_listing(None, false, &[], &fixture(), &mut |id| open_fake(&mut opened, id)).unwrap();
        assert!(opened.is_empty());
        assert_eq!(out["count"], 3);
        let row = &out["scanners"][0];
        assert_eq!(row["details"], "skipped");
        assert_eq!(row["id"], "{a}");
        assert_eq!(row["connection"], "wsd");
        assert!(row.get("feeder").is_none());
    }

    #[test]
    fn a_filter_opens_only_the_matching_devices() {
        let mut opened = Vec::new();
        let out = build_listing(Some("0000f2"), false, &[], &fixture(), &mut |id| open_fake(&mut opened, id)).unwrap();
        assert_eq!(opened, ["{a}"]);
        assert_eq!(out["count"], 1);
        assert_eq!(out["scanners"][0]["details"], "loaded");
        assert_eq!(out["scanners"][0]["feeder"], true);
    }

    #[test]
    fn an_empty_filter_is_an_error() {
        for text in ["", "  "] {
            let err = build_listing(Some(text), false, &[], &fixture(), &mut |_| Err(scan::ScanError::Unsupported)).unwrap_err();
            assert!(matches!(err, ToolError::InvalidArgs(_)), "{err}");
        }
    }

    #[test]
    fn details_opens_everything_and_keeps_a_failed_device_as_an_error_row() {
        let mut opened = Vec::new();
        let out = build_listing(None, true, &[], &fixture(), &mut |id| open_fake(&mut opened, id)).unwrap();
        assert_eq!(opened, ["{a}", "{b}", "{c}"]);
        assert_eq!(out["count"], 3);
        assert_eq!(out["scanners"][1]["details"], "loaded");
        let bad = &out["scanners"][2];
        assert_eq!(bad["details"], "error");
        assert_eq!(bad["error"], "The device is busy.");
        assert_eq!(bad["id"], "{c}");
    }

    #[test]
    fn denied_devices_are_never_opened_and_denied_filters_are_refused() {
        let denied = scan::parse_deny_list("99999999-aaaa-4bbb-8ccc-dddddddddddd");
        let mut opened = Vec::new();
        let out = build_listing(None, true, &denied, &fixture(), &mut |id| open_fake(&mut opened, id)).unwrap();
        assert_eq!(opened, ["{a}", "{c}"]);
        assert_eq!(out["scanners"][1]["details"], "denied");
        for text in ["99999999-aaaa-4bbb-8ccc-dddddddddddd", "99999999", "Office Printer"] {
            let result = build_listing(Some(text), true, &denied, &fixture(), &mut |id| open_fake(&mut opened, id));
            assert!(matches!(result, Err(ToolError::InvalidArgs(_))), "{text}: {result:?}");
        }
        // A filter that selects the denied device by another field is refused too.
        let err =
            build_listing(Some("printer"), false, &scan::parse_deny_list("office"), &fixture(), &mut |id| open_fake(&mut opened, id)).unwrap_err();
        assert!(matches!(err, ToolError::InvalidArgs(_)), "{err}");
        assert_eq!(opened, ["{a}", "{c}"]);
    }

    #[test]
    fn a_loaded_row_carries_all_nine_handling_flags_and_the_abnormal_ones_by_name() {
        let mut scanner = scanner_for("{a}");
        scanner.handling_status = scan::HandlingStatus::from_bits(0x1 | 0x20 | 0x400);
        let row = scanner_json(&scanner);
        let flags = row["handling_status"].as_object().unwrap();
        let names: Vec<&str> = flags.keys().map(String::as_str).collect();
        let mut expected: Vec<&str> = scan::HandlingStatus::FLAG_NAMES.to_vec();
        expected.sort_unstable();
        let mut got = names.clone();
        got.sort_unstable();
        assert_eq!(got, expected);
        assert_eq!(flags["jam"], true);
        assert_eq!(flags["lamp_error"], false);
        assert_eq!(row["conditions"], json!(["jam", "attention"]));
    }

    #[test]
    fn the_rows_and_the_schema_use_the_contract_lists() {
        for details in scan::ListDetails::ALL {
            assert_eq!(details.as_str().parse::<scan::ListDetails>().ok(), Some(*details));
        }
        let rows = build_listing(None, true, &scan::parse_deny_list("99999999"), &fixture(), &mut |id| open_fake(&mut Vec::new(), id)).unwrap();
        let seen: Vec<&str> = rows["scanners"].as_array().unwrap().iter().filter_map(|row| row["details"].as_str()).collect();
        assert_eq!(seen, ["loaded", "denied", "error"]);
        let def = crate::tools::find("scan_acquire").unwrap();
        let property = |name: &str| -> Vec<String> {
            def.input_schema["properties"][name]["enum"].as_array().unwrap().iter().filter_map(|v| v.as_str().map(str::to_string)).collect()
        };
        assert_eq!(property("source"), scan::Source::ALL.iter().map(|v| v.as_str().to_string()).collect::<Vec<_>>());
        assert_eq!(property("color"), scan::Color::ALL.iter().map(|v| v.as_str().to_string()).collect::<Vec<_>>());
    }

    #[test]
    fn a_failed_acquire_reports_its_identity_as_data() {
        let folder = std::env::temp_dir().join(format!("pdfcraft-scan-failure-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(&folder).unwrap();
        let settings = scan::ScanSettings { pages: 3, source: scan::Source::Feeder, ..scan::ScanSettings::default() };
        let busy = scan::ScanError::from_hresult(0x8021_0006, "");
        let Err(ToolError::FailedWith { message, data }) = acquire_failure(&folder, &settings, busy) else {
            panic!("a failure with no pages is a tool error");
        };
        assert!(message.contains("busy"), "{message}");
        assert_eq!(data["kind"], "busy");
        assert_eq!(data["phase"], "before_first_page");
        assert_eq!(data["hresult"], "0x80210006");
        assert_eq!(data["code_name"], "WIA_ERROR_BUSY");
        assert_eq!((data["retryable"].clone(), data["resumable"].clone()), (json!(true), json!(false)));
        let page = scan::Page { bytes: b"\x89PNG\r\n\x1a\nxx".to_vec(), format: "png", width: 2, height: 2, dpi: 200, thresholded: false };
        let jam = scan::ScanError::Incomplete {
            message: "The scanner has a paper jam.".into(),
            acquired: scan::Acquired { source: scan::Source::Feeder, pages: vec![page] },
            cause: Some(scan::WiaCause { code: 0x8021_0002, kind: scan::InterruptKind::Jam }),
        };
        let out = acquire_failure(&folder, &settings, jam).unwrap();
        assert_eq!(out["partial"], true);
        assert_eq!(out["error_info"]["kind"], "jam");
        assert_eq!(out["error_info"]["phase"], "between_pages");
        assert_eq!(out["error_info"]["code_name"], "WIA_ERROR_PAPER_JAM");
        assert_eq!((out["error_info"]["retryable"].clone(), out["error_info"]["resumable"].clone()), (json!(false), json!(true)));
        let _ = std::fs::remove_dir_all(&folder);
    }

    #[test]
    fn detail_by_exact_id_returns_one_row_and_refuses_denied_or_unknown() {
        let mut opened = Vec::new();
        let row = detail_row_for("{a}", &[], &fixture(), &mut |id| open_fake(&mut opened, id)).unwrap();
        assert_eq!(row["details"], "loaded");
        let row = detail_row_for("{c}", &[], &fixture(), &mut |id| open_fake(&mut opened, id)).unwrap();
        assert_eq!(row["details"], "error");
        assert!(detail_row_for("{zz}", &[], &fixture(), &mut |id| open_fake(&mut opened, id)).is_err());
        assert!(detail_row_for("{b}", &scan::parse_deny_list("office"), &fixture(), &mut |id| open_fake(&mut opened, id)).is_err());
        assert_eq!(opened, ["{a}", "{c}"]);
    }
}
