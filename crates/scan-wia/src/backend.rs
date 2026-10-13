//! The WIA 2.0 operations: listing, details, device dump and acquire. This module is safe code. It
//! reaches COM only through [`crate::ffi`] (single calls and variant reads) and [`crate::sinks`]
//! (the callback objects).

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use pdfcraft_scan::{
    Acquired, CancelToken, Color, Compression, Connection, DeviceDump, HandlingStatus, ImageChoices, ImageFormat, Intent, ItemDump, MAX_PAGES, Page,
    PageSize, PropertyInfo, Rotation, ScanError, ScanSettings, Scanner, ScannerRef, Source, ValueSpan, align_to_span, describe_windows_error,
    fit_region, handling_caps, listed_values, planned_frame, spanned_values,
};
use windows::Win32::Devices::ImageAcquisition::{
    DUPLEX, FEED_READY, FEEDER, FLATBED, IWiaDataTransfer, IWiaDevMgr, IWiaDevMgr2, IWiaItem, IWiaItem2, IWiaPropertyStorage, IWiaTransfer,
    TYMED_CALLBACK, WIA_AUTO_DESKEW_OFF, WIA_AUTO_DESKEW_ON, WIA_CATEGORY_FEEDER, WIA_CATEGORY_FEEDER_BACK, WIA_CATEGORY_FEEDER_FRONT,
    WIA_CATEGORY_FLATBED, WIA_COMPRESSION_BI_RLE4, WIA_COMPRESSION_BI_RLE8, WIA_COMPRESSION_G3, WIA_COMPRESSION_G4, WIA_COMPRESSION_JBIG,
    WIA_COMPRESSION_JPEG, WIA_COMPRESSION_JPEG2K, WIA_COMPRESSION_NONE, WIA_COMPRESSION_PNG, WIA_DIP_DEV_ID, WIA_DIP_DEV_NAME, WIA_DIP_PORT_NAME,
    WIA_DPS_DOCUMENT_HANDLING_CAPABILITIES, WIA_DPS_DOCUMENT_HANDLING_CAPACITY, WIA_DPS_DOCUMENT_HANDLING_SELECT, WIA_DPS_DOCUMENT_HANDLING_STATUS,
    WIA_DPS_HORIZONTAL_BED_SIZE, WIA_DPS_HORIZONTAL_SHEET_FEED_SIZE, WIA_DPS_OPTICAL_XRES, WIA_DPS_OPTICAL_YRES, WIA_DPS_PAGES,
    WIA_DPS_VERTICAL_BED_SIZE, WIA_DPS_VERTICAL_SHEET_FEED_SIZE, WIA_IPA_COMPRESSION, WIA_IPA_DATATYPE, WIA_IPA_DEPTH, WIA_IPA_FILENAME_EXTENSION,
    WIA_IPA_FORMAT, WIA_IPA_ITEM_CATEGORY, WIA_IPA_ITEM_FLAGS, WIA_IPA_ITEM_NAME, WIA_IPA_PREFERRED_FORMAT, WIA_IPA_TYMED, WIA_IPS_AUTO_DESKEW,
    WIA_IPS_BRIGHTNESS, WIA_IPS_CONTRAST, WIA_IPS_CUR_INTENT, WIA_IPS_INVERT, WIA_IPS_LAMP, WIA_IPS_MAX_HORIZONTAL_SIZE, WIA_IPS_MAX_VERTICAL_SIZE,
    WIA_IPS_MIRROR, WIA_IPS_ORIENTATION, WIA_IPS_PAGE_SIZE, WIA_IPS_PAGES, WIA_IPS_PHOTOMETRIC_INTERP, WIA_IPS_ROTATION, WIA_IPS_THRESHOLD,
    WIA_IPS_XEXTENT, WIA_IPS_XPOS, WIA_IPS_XRES, WIA_IPS_YEXTENT, WIA_IPS_YPOS, WIA_IPS_YRES, WIA_PROP_FLAG, WIA_PROP_LIST, WIA_PROP_RANGE,
    WIA_PROP_READ, WIA_PROP_WRITE, WiaDevMgr, WiaDevMgr2, WiaImgFmt_BMP, WiaImgFmt_JPEG, WiaImgFmt_JPEG2K, WiaImgFmt_PNG, WiaImgFmt_TIFF,
    WiaItemTypeTransfer,
};
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::Win32::System::Variant::{VT_BSTR, VT_CLSID, VT_I4, VT_LPWSTR, VT_UI2, VT_UI4};
use windows::core::{GUID, Interface};

use crate::ffi::{self, ComInit, CrossThreadTransfer};
use crate::sinks::{Bands, TransferState, band_bytes, band_callback, finish_band, halt_feeder, transfer_callback};

/// Cheap listing: id, name, port and connection kind from the device-info property storage only.
/// No device is opened, so nothing is sent to any scanner. The port is `None` when the info
/// storage does not publish it.
pub fn scanner_list() -> Result<Vec<ScannerRef>, ScanError> {
    let _com = ComInit::new().map_err(failed)?;
    let mgr = device_manager()?;
    let mut out = Vec::new();
    for info in enum_devices(&mgr)? {
        let id = read_string(&info, WIA_DIP_DEV_ID)?;
        let name = read_string(&info, WIA_DIP_DEV_NAME).unwrap_or_default();
        let port = read_string(&info, WIA_DIP_PORT_NAME).ok().filter(|text| !text.is_empty());
        out.push(ScannerRef::new(id, name, port));
    }
    Ok(out)
}

/// Full details for ONE device, by exact WIA id. This opens (CreateDevice) that device only.
pub fn scanner_details(id: &str) -> Result<Scanner, ScanError> {
    let _com = ComInit::new().map_err(failed)?;
    let mgr = device_manager()?;
    for info in enum_devices(&mgr)? {
        let found = read_string(&info, WIA_DIP_DEV_ID)?;
        if found == id {
            let name = read_string(&info, WIA_DIP_DEV_NAME).unwrap_or_default();
            return read_scanner(&mgr, &found, &name);
        }
    }
    Err(ScanError::Failed(format!("no scanner with id {id:?}")))
}

/// Connected scanners with full details. EXPENSIVE: opens every device. Prefer [`scanner_list`]
/// plus [`scanner_details`] for the one you need. A device that fails to open, or whose required
/// properties cannot be read, is omitted. The list is still returned.
pub fn scanners() -> Result<Vec<Scanner>, ScanError> {
    let _com = ComInit::new().map_err(failed)?;
    let mgr = device_manager()?;
    let mut out = Vec::new();
    for info in enum_devices(&mgr)? {
        let id = read_string(&info, WIA_DIP_DEV_ID)?;
        let name = read_string(&info, WIA_DIP_DEV_NAME).unwrap_or_default();
        // Open and drop one device before the next. Some WSD drivers fault if two are held.
        // An error from this device does not discard the devices already listed or still to come.
        if let Some(warning) = keep_opened(&mut out, &id, &name, read_scanner(&mgr, &id, &name)) {
            log::warn!("{warning}");
        }
    }
    Ok(out)
}

fn read_scanner(mgr: &IWiaDevMgr2, id: &str, name: &str) -> Result<Scanner, ScanError> {
    let device = open_device(mgr, id)?;
    let root = properties(&device)?;
    let caps = read_i4(&root, WIA_DPS_DOCUMENT_HANDLING_CAPABILITIES).unwrap_or(0);
    let status = read_i4(&root, WIA_DPS_DOCUMENT_HANDLING_STATUS).unwrap_or(0);
    let handling_status = HandlingStatus::from_bits(status);
    let (feeder, flatbed, duplex) = handling_caps(caps);
    let feeder_capacity = read_i4(&root, WIA_DPS_DOCUMENT_HANDLING_CAPACITY).ok().and_then(|n| u32::try_from(n).ok()).filter(|n| *n > 0);
    let port = read_string(&root, WIA_DIP_PORT_NAME).ok().filter(|text| !text.is_empty());
    let connection = port.as_deref().map(Connection::from_port).unwrap_or(Connection::Unknown);
    // WIA 2.0 keeps the optical resolution, the page range and the maximum scan area on the
    // transfer items (the root has only the handling bits); a WIA 1.0 style root is the fallback.
    let glass_item = scan_item(&device, Source::Glass).ok();
    let feeder_item = scan_item(&device, Source::Feeder).ok();
    let glass_props = glass_item.as_ref().and_then(|item| properties(item).ok());
    let feeder_props = feeder_item.as_ref().and_then(|item| properties(item).ok());
    let optical = |id: u32| {
        positive_u32(read_i4(&root, id).ok())
            .or_else(|| glass_props.as_ref().and_then(|props| positive_u32(read_i4(props, id).ok())))
            .or_else(|| feeder_props.as_ref().and_then(|props| positive_u32(read_i4(props, id).ok())))
    };
    let pages = choice_span(&root, WIA_DPS_PAGES).or_else(|| feeder_props.as_ref().and_then(|props| choice_span(props, WIA_IPS_PAGES)));
    let bed = glass_props.as_ref().and_then(|props| bed_area(props, &root)).or_else(|| bed_area(&root, &root));
    let sheet = feeder_props.as_ref().and_then(|props| sheet_area(props, &root)).or_else(|| sheet_area(&root, &root));
    Ok(Scanner {
        id: id.to_string(),
        name: name.to_string(),
        feeder,
        flatbed,
        duplex,
        feeder_ready: handling_status.feeder_ready,
        handling_status,
        feeder_capacity,
        connection,
        port,
        optical_x: optical(WIA_DPS_OPTICAL_XRES),
        optical_y: optical(WIA_DPS_OPTICAL_YRES),
        pages,
        bed,
        sheet,
        glass: glass_item.as_ref().map(image_choices),
        feeder_choices: feeder_item.as_ref().map(image_choices),
    })
}

/// Append a scanner that opened. A failed open is left out; the returned warning names it.
fn keep_opened<T>(out: &mut Vec<T>, id: &str, name: &str, opened: Result<T, ScanError>) -> Option<String> {
    match opened {
        Ok(scanner) => {
            out.push(scanner);
            None
        }
        Err(error) => Some(skip_open_warning(id, name, &error)),
    }
}

fn skip_open_warning(id: &str, name: &str, error: &ScanError) -> String {
    let device = if !name.is_empty() && !id.is_empty() && name != id {
        format!("{name} ({id})")
    } else if !name.is_empty() {
        name.to_string()
    } else if !id.is_empty() {
        id.to_string()
    } else {
        "scanner".to_string()
    };
    format!("scan: skipped {device}: {error}")
}

fn positive_u32(value: Option<i32>) -> Option<u32> {
    value.filter(|n| *n > 0).and_then(|n| u32::try_from(n).ok())
}

/// Connect and list every published property on the root and each transfer child. No Download.
pub fn dump_devices() -> Result<Vec<DeviceDump>, ScanError> {
    let _com = ComInit::new().map_err(failed)?;
    let mgr = device_manager()?;
    let mut out = Vec::new();
    for info in enum_devices(&mgr)? {
        let id = read_string(&info, WIA_DIP_DEV_ID)?;
        let name = read_string(&info, WIA_DIP_DEV_NAME).unwrap_or_default();
        // Open, dump, and drop one device before the next. Some WSD drivers fault if two are held.
        match dump_one_device(&mgr, &id, &name) {
            Ok(dump) => out.push(dump),
            Err(error) => out.push(DeviceDump {
                id,
                name,
                items: vec![ItemDump {
                    kind: "error".into(),
                    name: "dump".into(),
                    properties: vec![PropertyInfo { id: 0, name: "error".into(), vt: 0, flags: 0, value: error.to_string(), access: "none".into() }],
                }],
            }),
        }
    }
    Ok(out)
}

fn dump_one_device(mgr: &IWiaDevMgr2, id: &str, name: &str) -> Result<DeviceDump, ScanError> {
    let device = open_device(mgr, id)?;
    let mut items = vec![ItemDump { kind: "root".into(), name: "root".into(), properties: dump_properties(&properties(&device)?) }];
    for child in enum_children(&device)? {
        let item_name = item_name(&child);
        let kind = if source_matches(Source::Feeder, item_category(&child).as_ref(), &item_name) {
            "feeder"
        } else if source_matches(Source::Glass, item_category(&child).as_ref(), &item_name) {
            "glass"
        } else {
            "child"
        };
        items.push(ItemDump { kind: kind.into(), name: item_name, properties: dump_properties(&properties(&child)?) });
    }
    Ok(DeviceDump { id: id.to_string(), name: name.to_string(), items })
}

thread_local! {
    /// Set when [`failed`] already wrote the warn line (with the HRESULT).
    static WIA_FAILURE_LOGGED: Cell<bool> = const { Cell::new(false) };
}

pub fn acquire(settings: &ScanSettings) -> Result<Acquired, ScanError> {
    WIA_FAILURE_LOGGED.set(false);
    log_acquire_start(settings);
    let route = Cell::new(None);
    let mut result = acquire_pages(settings, &route);
    if let (Err(error), Some((resolved, has_glass))) = (&result, route.get())
        && pdfcraft_scan::auto_fallback_to_glass(settings.source, resolved, has_glass, pdfcraft_scan::pages_in_error(error), error)
    {
        // Drivers keep FEED_READY set with an empty tray, so Auto picks the feeder and only the
        // transfer finds it empty (after the printer's own ~10 s timeout). Try the glass once.
        let reason = if error.interrupt_kind() == pdfcraft_scan::InterruptKind::FeederEmpty { "feeder_empty" } else { "busy_or_feeder_empty" };
        log::info!("scan: auto: the feeder had no paper ({reason}); trying the glass");
        let trace = pdfcraft_scan::trace::AcquireTrace::from_env();
        if trace.enabled() {
            trace.phase("auto_fallback", &[("from", "feeder"), ("to", "glass"), ("reason", reason)]);
        }
        let mut glass = settings.clone();
        glass.source = Source::Glass;
        glass.pages = 1;
        glass.duplex = false;
        result = acquire_pages(&glass, &Cell::new(None));
    }
    match &result {
        Ok(acquired) => log_acquire_ok(acquired),
        Err(error) => {
            if !WIA_FAILURE_LOGGED.get() {
                log::warn!("scan: {error}");
            }
        }
    }
    result
}

fn log_acquire_start(settings: &ScanSettings) {
    let device = if settings.device.is_empty() { "default" } else { settings.device.as_str() };
    let region =
        settings.region.map_or_else(|| "full".to_string(), |region| format!("{}x{}+{}+{}", region.width, region.height, region.left, region.top));
    log::debug!(
        "scan: acquire device={device} source={} dpi={} dpi_y={:?} color={} pages={} brightness={:?} contrast={:?} threshold={:?} rotation={:?} format={:?} compression={:?} intent={:?} page_size={:?} auto_deskew={:?} orientation={:?} photometric={:?} invert={:?} mirror={:?} lamp={:?} duplex={} region={region}",
        settings.source.name(),
        settings.dpi,
        settings.dpi_y,
        settings.color.name(),
        settings.pages,
        settings.brightness,
        settings.contrast,
        settings.threshold,
        settings.rotation.map(Rotation::name),
        settings.format.as_deref(),
        settings.compression.as_deref(),
        settings.intent.map(Intent::name),
        settings.page_size.map(PageSize::name),
        settings.auto_deskew,
        settings.orientation,
        settings.photometric,
        settings.invert,
        settings.mirror,
        settings.lamp,
        settings.duplex,
    );
}

fn log_acquire_ok(acquired: &Acquired) {
    let bytes: usize = acquired.pages.iter().map(|page| page.bytes.len()).sum();
    let size = acquired.pages.first().map(|page| format!("{}x{}", page.width, page.height)).unwrap_or_else(|| "0x0".into());
    let dpi = acquired.pages.first().map(|page| page.dpi).unwrap_or(0);
    log::info!("scan: {} page(s) from {} at {} dpi ({size}, {bytes} bytes)", acquired.pages.len(), acquired.source.name(), dpi,);
}

/// `route` receives the resolved source and whether the glass exists, as soon as they are known.
fn acquire_pages(settings: &ScanSettings, route: &Cell<Option<(Source, bool)>>) -> Result<Acquired, ScanError> {
    let settings = settings.checked()?;
    let trace = pdfcraft_scan::trace::AcquireTrace::from_env();
    let _com = ComInit::new().map_err(failed)?;
    let mgr = device_manager()?;
    let devices = enum_devices(&mgr)?;
    let chosen = choose(&devices, &settings.device)?;
    let id = read_string(chosen, WIA_DIP_DEV_ID)?;
    // The WIA 2.0 document-handling property on this driver stays at front-only and rejects the bed.
    // The compatible device accepts flatbed and feeder, which is what keeps paper in the feeder from
    // being picked up when the scan is from the glass.
    let (root_caps, status_bits, wsd_port) = {
        let probe = open_device(&mgr, &id)?;
        let root = properties(&probe)?;
        let port_name = read_string(&root, WIA_DIP_PORT_NAME).ok();
        let dev_name = read_string(&root, WIA_DIP_DEV_NAME).unwrap_or_default();
        let wsd = port_name.as_deref().map(Connection::from_port) == Some(Connection::Wsd)
            || settings.device.to_ascii_lowercase().contains("wsd")
            || dev_name.to_ascii_lowercase().contains("wsd");
        (read_i4(&root, WIA_DPS_DOCUMENT_HANDLING_CAPABILITIES).unwrap_or(0), read_i4(&root, WIA_DPS_DOCUMENT_HANDLING_STATUS).unwrap_or(0), wsd)
    };
    let (has_feeder, has_glass, duplex_cap) = handling_caps(root_caps);
    let handling_status = HandlingStatus::from_bits(status_bits);
    if trace.enabled() {
        trace.status("probe", handling_status);
    }
    if let Some(reason) = handling_status.acquire_block_reason() {
        return Err(ScanError::Failed(reason.into()));
    }
    let feeder_ready = flag(status_bits, FEED_READY);
    // Auto becomes feeder or glass from this status read before any transfer.
    let source = settings.source.resolve(feeder_ready, has_feeder, has_glass)?;
    route.set(Some((source, has_glass)));
    if let Some(progress) = &settings.progress {
        progress.set_source(source);
    }
    if settings.duplex && !duplex_cap {
        return Err(ScanError::Failed("duplex is not offered".into()));
    }
    if settings.duplex && source != Source::Feeder {
        return Err(ScanError::Failed("duplex needs the feeder".into()));
    }
    // Keep the WIA 1.0 device that accepted the select open for the whole transfer. Dropping it
    // before Download let some drivers fall back to the feeder when FEED_READY was still set, so a
    // glass scan could pull the tray. The inbox WSD driver can reject the WIA 1.0 device open;
    // the WIA 2.0 select below still runs.
    let _handling = if wsd_port {
        // Inbox / lab WSD: WIA 1.0 CreateDevice and DOCUMENT_HANDLING_SELECT return E_POINTER.
        None
    } else {
        match select_source(&id, source, settings.duplex) {
            Ok(hold) => Some(hold),
            Err(error) => {
                log::warn!("scan: WIA 1.0 source select was not accepted ({error})");
                None
            }
        }
    };
    let device = open_device(&mgr, &id)?;
    let root = properties(&device)?;
    let connection = read_string(&root, WIA_DIP_PORT_NAME)
        .ok()
        .filter(|text| !text.is_empty())
        .map(|port| Connection::from_port(&port))
        .unwrap_or(Connection::Unknown);
    if trace.enabled() {
        trace.phase("device_open", &[("connection", connection.name())]);
    }
    if wsd_port {
        if let Ok(value) = handling_select_value(source, settings.duplex) {
            write_i4_soft(&root, WIA_DPS_DOCUMENT_HANDLING_SELECT, value)?;
        }
    } else {
        let _ = write_handling_select(&root, source, settings.duplex);
    }
    let status_now = HandlingStatus::from_bits(read_i4(&root, WIA_DPS_DOCUMENT_HANDLING_STATUS).unwrap_or(0));
    if trace.enabled() {
        trace.status("preflight", status_now);
    }
    if let Some(reason) = status_now.acquire_block_reason() {
        return Err(ScanError::Failed(reason.into()));
    }
    if pdfcraft_scan::feeder_empty_blocks_acquire(source, status_now.feeder_ready, wsd_port) {
        return Err(ScanError::Failed("the feeder is empty".into()));
    }
    if source == Source::Feeder && !status_now.feeder_ready && wsd_port {
        log::warn!("scan: WSD feeder status is empty; continuing feeder acquire");
    }
    let child = scan_item(&device, source)?;
    let item = properties(&child)?;
    if let Some(intent) = settings.intent {
        require_choice(&item, WIA_IPS_CUR_INTENT, intent.driver(), "intent")?;
        write_i4(&item, WIA_IPS_CUR_INTENT, intent.driver())?;
    }
    if let Some(page_size) = settings.page_size {
        require_choice(&item, WIA_IPS_PAGE_SIZE, page_size.driver(), "page size")?;
        write_i4(&item, WIA_IPS_PAGE_SIZE, page_size.driver())?;
    }
    if let Some(on) = settings.auto_deskew {
        let value = as_i32(if on { WIA_AUTO_DESKEW_ON } else { WIA_AUTO_DESKEW_OFF })?;
        require_choice(&item, WIA_IPS_AUTO_DESKEW, value, "auto-deskew")?;
        write_i4(&item, WIA_IPS_AUTO_DESKEW, value)?;
    }
    if let Some(format) = settings.format.as_deref() {
        write_format(&item, format)?;
    }
    if let Some(compression) = settings.compression.as_deref() {
        write_compression(&item, compression)?;
    }
    let from_dpi = u32::try_from(read_i4(&item, WIA_IPS_XRES).unwrap_or(200)).unwrap_or(200).max(1);
    let old_x = u32::try_from(read_i4(&item, WIA_IPS_XEXTENT).unwrap_or(0)).unwrap_or(0);
    let old_y = u32::try_from(read_i4(&item, WIA_IPS_YEXTENT).unwrap_or(0)).unwrap_or(0);
    let max = match source {
        Source::Glass => bed_area(&item, &root),
        Source::Feeder => sheet_area(&item, &root),
        Source::Auto => None,
    };
    let dpi_y = settings.dpi_y.unwrap_or(settings.dpi);
    let frame = if let Some(region) = settings.region {
        // Refuse a window that cannot be drawn before any resolution write.
        let fitted = fit_region(region, max)?;
        planned_frame(old_x, old_y, from_dpi, settings.dpi, Some(fitted), None)?
    } else if old_x == 0 || old_y == 0 {
        // Some WSD items leave the extent at 0 until a window is written. The bed size is
        // already in thousandths of an inch, which is the unit a region uses. When the bed
        // size is missing too, leave the driver's own page size rather than writing 0.
        if let Some((width, height)) = max {
            let full = pdfcraft_scan::Region { left: 0, top: 0, width, height };
            planned_frame(old_x, old_y, from_dpi, settings.dpi, Some(full), None)?
        } else {
            // The installed WSD item can be a named Flatbed whose format and extent never
            // arrived. Writing them on the root does not make Download start a job.
            // Distinct from offline (0x80210005): the device opens, but the item is empty.
            log::warn!("scan: empty Flatbed item (no extent, no bed size); re-Add in Windows — not the same as offline");
            return Err(ScanError::Failed("the scanner has no page size (empty item). Remove it in Windows and add it again while it is on.".into()));
        }
    } else {
        planned_frame(old_x, old_y, from_dpi, settings.dpi, None, None)?
    };
    let dpi_x = as_i32(settings.dpi)?;
    let dpi_y_i = as_i32(dpi_y)?;
    require_choice(&item, WIA_IPS_XRES, dpi_x, "dpi")?;
    require_choice(&item, WIA_IPS_YRES, dpi_y_i, "dpi-y")?;
    write_i4_if_needed(&item, WIA_IPS_XRES, dpi_x, wsd_port)?;
    write_i4_if_needed(&item, WIA_IPS_YRES, dpi_y_i, wsd_port)?;
    let (x, y) = if frame.width == 0 || frame.height == 0 {
        (0, 0)
    } else if frame.move_origin {
        // Resolution first, then the window in pixels at that resolution.
        let _ = write_stepped(&item, WIA_IPS_XPOS, frame.x, false, wsd_port)?;
        let _ = write_stepped(&item, WIA_IPS_YPOS, frame.y, false, wsd_port)?;
        (write_stepped(&item, WIA_IPS_XEXTENT, frame.width, true, wsd_port)?, write_stepped(&item, WIA_IPS_YEXTENT, frame.height, true, wsd_port)?)
    } else if wsd_port {
        write_i4_soft(&item, WIA_IPS_XEXTENT, as_i32(frame.width)?)?;
        write_i4_soft(&item, WIA_IPS_YEXTENT, as_i32(frame.height)?)?;
        (frame.width, frame.height)
    } else {
        write_i4(&item, WIA_IPS_XEXTENT, as_i32(frame.width)?)?;
        write_i4(&item, WIA_IPS_YEXTENT, as_i32(frame.height)?)?;
        (frame.width, frame.height)
    };
    require_color(&item, settings.color)?;
    if wsd_port {
        write_i4_soft(&item, WIA_IPA_DATATYPE, settings.color.data_type())?;
        write_i4_soft(&item, WIA_IPA_DEPTH, as_i32(settings.color.depth())?)?;
    } else {
        write_i4_if_needed(&item, WIA_IPA_DATATYPE, settings.color.data_type(), false)?;
        write_i4_if_needed(&item, WIA_IPA_DEPTH, as_i32(settings.color.depth())?, false)?;
    }
    write_optional(&item, WIA_IPS_BRIGHTNESS, settings.brightness, "brightness")?;
    write_optional(&item, WIA_IPS_CONTRAST, settings.contrast, "contrast")?;
    write_optional(&item, WIA_IPS_THRESHOLD, settings.threshold, "threshold")?;
    if let Some(rotation) = settings.rotation {
        require_choice(&item, WIA_IPS_ROTATION, rotation.driver(), "rotation")?;
        write_i4(&item, WIA_IPS_ROTATION, rotation.driver())?;
    }
    if let Some(orientation) = settings.orientation {
        require_choice(&item, WIA_IPS_ORIENTATION, orientation, "orientation")?;
        write_i4(&item, WIA_IPS_ORIENTATION, orientation)?;
    }
    if let Some(photometric) = settings.photometric {
        require_choice(&item, WIA_IPS_PHOTOMETRIC_INTERP, photometric, "photometric")?;
        write_i4(&item, WIA_IPS_PHOTOMETRIC_INTERP, photometric)?;
    }
    write_flag_bool(&item, WIA_IPS_INVERT, settings.invert, "invert")?;
    write_flag_bool(&item, WIA_IPS_MIRROR, settings.mirror, "mirror")?;
    if let Some(on) = settings.lamp {
        // WIA_LAMP_ON = 0, WIA_LAMP_OFF = 1.
        let value = if on { 0 } else { 1 };
        require_choice(&item, WIA_IPS_LAMP, value, "lamp")?;
        write_i4(&item, WIA_IPS_LAMP, value)?;
    }
    // Resolution writes can reset the page count, so it is written last, on the item that transfers.
    // Zero is the driver's own "whole feeder". The bed is one page.
    // Auto may have carried a feeder page count; once resolved to the bed, take one page.
    let page_count = match source {
        Source::Glass if settings.pages > 1 && settings.source == Source::Glass => {
            return Err(ScanError::Failed("the bed scans one page".into()));
        }
        Source::Glass => 1,
        Source::Feeder if settings.pages == 0 => 0,
        Source::Feeder => i32::try_from(settings.pages).unwrap_or(i32::MAX),
        Source::Auto => return Err(ScanError::Failed("the scan source was not resolved".into())),
    };
    if source == Source::Feeder {
        // The page count belongs on the feeder item. Some WIA 2.0 roots keep Document Handling
        // Select on the bed and reject a multi-page write there even when the feeder child's range
        // allows it; the root write is best-effort after the item accepts.
        if wsd_port {
            // Inbox WSD often omits IPS_PAGES on the transfer item; the validated ticket already
            // carries ImagesToTransfer. Hard writes return E_POINTER / invalid parameter.
            // `pages=0` (whole feeder): some WSD drivers map IPS_PAGES=0 to one sheet; use a large cap.
            let ticket_pages = pdfcraft_scan::wsd_feeder_ticket_pages(page_count);
            if page_count == 0 {
                write_i4_soft(&item, WIA_IPS_PAGES, ticket_pages)?;
                write_i4_soft(&root, WIA_DPS_PAGES, ticket_pages)?;
            } else {
                let _ = require_choice(&item, WIA_IPS_PAGES, ticket_pages, "pages");
                write_i4_soft(&item, WIA_IPS_PAGES, ticket_pages)?;
                write_i4_soft(&root, WIA_DPS_PAGES, ticket_pages)?;
            }
        } else {
            require_choice(&item, WIA_IPS_PAGES, page_count, "pages")?;
            write_i4(&item, WIA_IPS_PAGES, page_count)?;
            let _ = write_i4(&root, WIA_DPS_PAGES, page_count);
            if read_i4(&item, WIA_IPS_PAGES).unwrap_or(-1) != page_count {
                return Err(ScanError::Failed("the feeder did not accept the page count, so the scan was not started".into()));
            }
        }
    }

    if trace.enabled() {
        let dpi_s = settings.dpi.to_string();
        let pages_s = settings.pages.to_string();
        trace.phase(
            "props_written",
            &[
                ("source", source.name()),
                ("color", settings.color.name()),
                ("dpi", &dpi_s),
                ("pages_requested", &pages_s),
                ("connection", connection.name()),
                ("wsd", if wsd_port { "true" } else { "false" }),
            ],
        );
    }

    let wanted_n = if page_count == 0 { usize::try_from(MAX_PAGES).unwrap_or(0) } else { usize::try_from(page_count).unwrap_or(0) };
    // Media type is a whole file or a banded callback. Inbox WSD (wsdscdrv) accepts a
    // TYMED_CALLBACK (128) write, starts CreateScanJob, then fails the banded transfer
    // before RetrieveImage. A following IWiaTransfer::Download hits that Pending job and
    // returns Unspecified with no RetrieveImage on inbox WSD. WIA Automation
    // `Item.Transfer()` (file) succeeds on the same device. Never attempt callback banding
    // when the port is WSD — go straight to IWiaTransfer::Download with TYMED_FILE.
    // Non-WSD: only band when the callback write sticks.
    let previous_tymed = read_i4(&item, WIA_IPA_TYMED).unwrap_or(2);
    let callback = i32::try_from(TYMED_CALLBACK).unwrap_or(128);
    let banded_ready =
        if wsd_port { false } else { write_i4(&item, WIA_IPA_TYMED, callback).is_ok() && read_i4(&item, WIA_IPA_TYMED).unwrap_or(-1) == callback };
    let _restore = TymedRestore { item: &item, value: previous_tymed };
    let on_feeder = source == Source::Feeder;
    if banded_ready {
        if let Ok(data) = child.cast::<IWiaDataTransfer>() {
            let pages_requested = if settings.duplex { 0 } else { usize::try_from(settings.pages).unwrap_or(0) };
            if trace.enabled() {
                trace.phase("transfer_start", &[("mode", "banded"), ("source", source.name())]);
            }
            match receive_bands(
                &data,
                x,
                y,
                settings.color.depth(),
                pdfcraft_scan::bw::PageRule::for_settings(settings),
                wanted_n,
                pages_requested,
                settings.progress.clone(),
                &root,
                on_feeder,
            ) {
                Ok(pages) => {
                    if trace.enabled() {
                        let pages_n = pages.len().to_string();
                        trace.phase("transfer_end", &[("mode", "banded"), ("outcome", "ok"), ("pages", &pages_n)]);
                    }
                    return Ok(Acquired { source, pages });
                }
                Err(error) => {
                    if trace.enabled() {
                        trace.phase("transfer_end", &[("mode", "banded"), ("outcome", "error")]);
                    }
                    let status_after = HandlingStatus::from_bits(read_i4(&root, WIA_DPS_DOCUMENT_HANDLING_STATUS).unwrap_or(0));
                    let error = pdfcraft_scan::explain_transfer_failure(error, status_after, on_feeder, 0, pages_requested);
                    if pdfcraft_scan::retry_with_file_transfer(&error) {
                        log::warn!("scan: banded transfer did not return a page ({error}); trying a file transfer");
                        let _ = write_i4(&item, WIA_IPA_TYMED, previous_tymed);
                    } else {
                        return Err(error);
                    }
                }
            }
        }
    } else if wsd_port {
        // Inbox WSD: keep TYMED_FILE (2) for IWiaTransfer::Download (never leave CALLBACK).
        let _ = write_i4(&item, WIA_IPA_TYMED, 2);
        log::warn!("scan: WSD port — skipping TYMED callback; using file transfer");
    } else {
        log::warn!("scan: TYMED callback ({callback}) not accepted; using file transfer");
    }
    if let Some(progress) = &settings.progress {
        if wsd_port {
            // Compressed MTOM size is unknown up front; BMP estimate would pin the bar at ~3%.
            progress.set_expected_bytes(None);
        } else {
            progress.set_expected_bytes(pdfcraft_scan::estimated_bmp_bytes(x, y, settings.color.depth()));
        }
    }

    let state = Rc::new(RefCell::new(TransferState {
        pages: Vec::new(),
        stream: None,
        path: None,
        offered: 0,
        limit: wanted_n,
        transfer: None,
        device: None,
        feeder: source == Source::Feeder,
        rule: pdfcraft_scan::bw::PageRule::for_settings(settings),
        cancel: settings.cancel.clone(),
        progress: settings.progress.clone(),
        wire_byte_progress: wsd_port,
    }));
    let sink = transfer_callback(&state);
    let transfer: IWiaTransfer = child.cast().map_err(failed)?;
    {
        let mut guard = state.try_borrow_mut().map_err(|_| ScanError::Failed("the transfer state was still borrowed".into()))?;
        guard.transfer = Some(transfer.clone());
        guard.device = Some(device.clone());
    }
    if let Some(token) = &settings.cancel {
        // Keep an owned clone for Cancel from another thread; CancelTransferClear drops it.
        let remote = CrossThreadTransfer(transfer.clone());
        token.set_canceller(std::sync::Arc::new(move || {
            let _ = remote.cancel();
        }));
    }
    let _clear_cancel = CancelTransferClear(settings.cancel.clone());
    if settings.cancel.as_ref().is_some_and(pdfcraft_scan::CancelToken::is_requested) {
        halt_feeder(&state);
        return Err(ScanError::Failed("the scan was cancelled".into()));
    }
    if trace.enabled() {
        trace.phase("transfer_start", &[("mode", "file"), ("source", source.name()), ("wsd", if wsd_port { "true" } else { "false" })]);
    }
    // Cancel after the last requested image. A driver error from that cancel is the stop.
    let download = ffi::download(&transfer, &sink);
    // Some ADF drivers beep for STOP_FEEDER / Cancel, then keep the stack moving. Call again after
    // Download returns so a following glass scan does not meet a still-busy feeder.
    if source == Source::Feeder && page_count > 0 {
        halt_feeder(&state);
    }
    let mut pages = {
        let mut guard = state.try_borrow_mut().map_err(|_| ScanError::Failed("the transfer state was still borrowed".into()))?;
        std::mem::take(&mut guard.pages)
    };
    let cancelled = settings.cancel.as_ref().is_some_and(pdfcraft_scan::CancelToken::is_requested);
    let status_after = HandlingStatus::from_bits(read_i4(&root, WIA_DPS_DOCUMENT_HANDLING_STATUS).unwrap_or(0));
    if trace.enabled() {
        trace.status("after_transfer", status_after);
        let outcome = if cancelled {
            "cancelled"
        } else if download.is_err() {
            "error"
        } else {
            "ok"
        };
        let pages_n = pages.len().to_string();
        trace.phase("transfer_end", &[("mode", "file"), ("outcome", outcome), ("pages", &pages_n)]);
    }
    let received = pages.len();
    let download_error = download.err().map(failed).map(|error| {
        let pages_requested = usize::try_from(settings.pages).unwrap_or(0);
        pdfcraft_scan::explain_transfer_failure(error, status_after, on_feeder, received, pages_requested)
    });
    // Hardware duplex counts sides, not sheets, so the page-count check is skipped there.
    let checked_pages = if settings.duplex { 0 } else { usize::try_from(settings.pages).unwrap_or(0) };
    if let Err(error) = pdfcraft_scan::after_transfer_checked(checked_pages, on_feeder, received, cancelled, download_error) {
        if pages.is_empty() {
            return Err(error);
        }
        pages.truncate(wanted_n);
        let acquired = Acquired { source, pages };
        return Err(ScanError::Incomplete { message: error.to_string(), cause: error.cause(), acquired });
    }
    pages.truncate(wanted_n);
    if pages.is_empty() {
        return Err(ScanError::Failed("the scanner transferred no pages".into()));
    }
    Ok(Acquired { source, pages })
}

struct CancelTransferClear(Option<CancelToken>);

impl Drop for CancelTransferClear {
    fn drop(&mut self) {
        if let Some(token) = &self.0 {
            // Drops the owned transfer clone stored for cross-thread Cancel.
            token.clear_canceller();
        }
    }
}

/// Keeps the WIA 1.0 device that accepted [`WIA_DPS_DOCUMENT_HANDLING_SELECT`] alive.
struct HandlingHold {
    _item: IWiaItem,
}

fn handling_select_value(source: Source, duplex: bool) -> Result<i32, ScanError> {
    let mut value = match source {
        Source::Feeder => FEEDER,
        Source::Glass => FLATBED,
        Source::Auto => return Err(ScanError::Failed("the scan source was not resolved".into())),
    };
    if duplex {
        value |= DUPLEX;
    }
    as_i32(value)
}

fn write_handling_select(props: &IWiaPropertyStorage, source: Source, duplex: bool) -> Result<(), ScanError> {
    let value = handling_select_value(source, duplex)?;
    write_i4(props, WIA_DPS_DOCUMENT_HANDLING_SELECT, value)?;
    if read_i4(props, WIA_DPS_DOCUMENT_HANDLING_SELECT).unwrap_or(-1) != value {
        return Err(match source {
            Source::Glass => ScanError::Failed("the scanner did not select the bed".into()),
            Source::Feeder if duplex => ScanError::Failed("the scanner did not select duplex".into()),
            Source::Feeder => ScanError::Failed("the scanner did not select the feeder".into()),
            Source::Auto => ScanError::Failed("the scan source was not resolved".into()),
        });
    }
    Ok(())
}

fn select_source(id: &str, source: Source, duplex: bool) -> Result<HandlingHold, ScanError> {
    let mgr: IWiaDevMgr = ffi::create_instance(&WiaDevMgr).map_err(failed)?;
    let item: IWiaItem = ffi::create_legacy_device(&mgr, id).map_err(failed)?;
    let props: IWiaPropertyStorage = item.cast().map_err(failed)?;
    write_handling_select(&props, source, duplex)?;
    Ok(HandlingHold { _item: item })
}

fn device_manager() -> Result<IWiaDevMgr2, ScanError> {
    ffi::create_instance(&WiaDevMgr2).map_err(failed)
}

fn open_device(mgr: &IWiaDevMgr2, id: &str) -> Result<IWiaItem2, ScanError> {
    ffi::create_device(mgr, id).map_err(failed)
}

fn enum_devices(mgr: &IWiaDevMgr2) -> Result<Vec<IWiaPropertyStorage>, ScanError> {
    ffi::device_infos(mgr).map_err(failed)?.map(|info| info.map_err(failed)).collect()
}

fn choose<'a>(devices: &'a [IWiaPropertyStorage], wanted: &str) -> Result<&'a IWiaPropertyStorage, ScanError> {
    if devices.is_empty() {
        return Err(ScanError::Failed("no scanner is connected".into()));
    }
    if wanted.is_empty() {
        return devices.first().ok_or_else(|| ScanError::Failed("no scanner is connected".into()));
    }
    let mut matched = None;
    for device in devices {
        let id = read_string(device, WIA_DIP_DEV_ID).unwrap_or_default();
        let name = read_string(device, WIA_DIP_DEV_NAME).unwrap_or_default();
        if id == wanted || name == wanted || name.contains(wanted) || id.contains(wanted) {
            if matched.is_some() {
                return Err(ScanError::Failed(format!("{wanted:?} matches more than one scanner")));
            }
            matched = Some(device);
        }
    }
    matched.ok_or_else(|| ScanError::Failed(format!("no scanner matches {wanted:?}")))
}

fn enum_children(root: &IWiaItem2) -> Result<Vec<IWiaItem2>, ScanError> {
    ffi::child_items(root).map_err(failed)?.map(|item| item.map_err(failed)).collect()
}

fn scan_item(root: &IWiaItem2, source: Source) -> Result<IWiaItem2, ScanError> {
    let transfer_bit = as_i32(WiaItemTypeTransfer)?;
    let mut items = Vec::new();
    for item in ffi::child_items(root).map_err(failed)? {
        let item = item.map_err(failed)?;
        let flags = read_i4(&properties(&item)?, WIA_IPA_ITEM_FLAGS).unwrap_or(0);
        if flags & transfer_bit != 0 {
            items.push(item);
        }
    }
    let chosen = items
        .iter()
        .find(|item| source_matches(source, item_category(item).as_ref(), &item_name(item)))
        .or_else(|| items.first().filter(|_| items.len() == 1));
    chosen.cloned().ok_or_else(|| match source {
        Source::Feeder => ScanError::Failed("this scanner has no feeder".into()),
        Source::Glass => ScanError::Failed("this scanner has no glass".into()),
        Source::Auto => ScanError::Failed("the scan source was not resolved".into()),
    })
}

fn source_matches(source: Source, category: Option<&GUID>, name: &str) -> bool {
    let feeder_name = name.eq_ignore_ascii_case("feeder");
    let glass_name = name.eq_ignore_ascii_case("flatbed") || name.eq_ignore_ascii_case("glass");
    match source {
        Source::Feeder => {
            category.is_some_and(|id| id == &WIA_CATEGORY_FEEDER || id == &WIA_CATEGORY_FEEDER_FRONT || id == &WIA_CATEGORY_FEEDER_BACK)
                || feeder_name
        }
        Source::Glass => category.is_some_and(|id| id == &WIA_CATEGORY_FLATBED) || glass_name,
        Source::Auto => false,
    }
}

fn item_name(item: &IWiaItem2) -> String {
    properties(item).ok().and_then(|props| read_string(&props, WIA_IPA_ITEM_NAME).ok()).unwrap_or_default()
}

fn item_category(item: &IWiaItem2) -> Option<GUID> {
    read_guid(&properties(item).ok()?, WIA_IPA_ITEM_CATEGORY)
}

fn image_choices(item: &IWiaItem2) -> ImageChoices {
    let Ok(props) = properties(item) else {
        return ImageChoices::default();
    };
    let dpi = choice_list(&props, WIA_IPS_XRES).into_iter().filter_map(|value| u32::try_from(value).ok()).collect();
    let dpi_y = choice_list(&props, WIA_IPS_YRES).into_iter().filter_map(|value| u32::try_from(value).ok()).collect();
    let colors = choice_list(&props, WIA_IPA_DATATYPE).into_iter().filter_map(Color::from_data_type).collect();
    let rotation = choice_list(&props, WIA_IPS_ROTATION).into_iter().filter_map(Rotation::from_driver).collect();
    let formats = format_names(&props);
    let compression = choice_list(&props, WIA_IPA_COMPRESSION).into_iter().filter_map(compression_name).map(str::to_string).collect();
    let intents = intent_choices(&props);
    let page_sizes = choice_list(&props, WIA_IPS_PAGE_SIZE).into_iter().filter_map(PageSize::from_driver).collect();
    let auto_deskew = read_i4(&props, WIA_IPS_AUTO_DESKEW).is_ok();
    let orientation = choice_list(&props, WIA_IPS_ORIENTATION);
    let photometric = choice_list(&props, WIA_IPS_PHOTOMETRIC_INTERP);
    let invert = read_i4(&props, WIA_IPS_INVERT).is_ok();
    let mirror = read_i4(&props, WIA_IPS_MIRROR).is_ok();
    let lamp = read_i4(&props, WIA_IPS_LAMP).is_ok();
    ImageChoices {
        dpi,
        dpi_y,
        colors,
        brightness: choice_span(&props, WIA_IPS_BRIGHTNESS),
        contrast: choice_span(&props, WIA_IPS_CONTRAST),
        threshold: choice_span(&props, WIA_IPS_THRESHOLD),
        rotation,
        formats,
        compression,
        intents,
        page_sizes,
        auto_deskew,
        orientation,
        photometric,
        invert,
        mirror,
        lamp,
    }
}

fn intent_choices(props: &IWiaPropertyStorage) -> Vec<Intent> {
    let Ok((flags, raw)) = property_numbers(props, WIA_IPS_CUR_INTENT) else {
        return Vec::new();
    };
    if flags & WIA_PROP_LIST != 0 {
        return listed_values(&raw).into_iter().filter_map(Intent::from_driver).collect();
    }
    if flags & WIA_PROP_FLAG != 0 {
        // Some drivers publish `[nominal, allowed_mask]` (e.g. `0,196615`).
        return intents_from_flag_mask(raw.last().copied().unwrap_or(0));
    }
    Vec::new()
}

/// Intents whose driver bits sit in a published flag mask.
fn intents_from_flag_mask(mask: i32) -> Vec<Intent> {
    [Intent::Color, Intent::Gray, Intent::Text, Intent::MinimizeSize, Intent::MaximizeQuality, Intent::BestPreview]
        .into_iter()
        .filter(|intent| mask & intent.driver() != 0)
        .collect()
}

fn format_names(props: &IWiaPropertyStorage) -> Vec<String> {
    let mut names = guid_list(props, WIA_IPA_FORMAT).into_iter().filter_map(format_name).map(str::to_string).collect::<Vec<_>>();
    if names.is_empty()
        && let Some(name) = read_guid(props, WIA_IPA_PREFERRED_FORMAT).and_then(format_name).map(str::to_string)
    {
        names.push(name);
    }
    if names.is_empty()
        && let Ok(ext) = read_string(props, WIA_IPA_FILENAME_EXTENSION)
    {
        let ext = ext.trim().trim_start_matches('.').to_ascii_lowercase();
        if !ext.is_empty() {
            names.push(ext);
        }
    }
    names.sort();
    names.dedup();
    names
}

fn write_compression(props: &IWiaPropertyStorage, name: &str) -> Result<(), ScanError> {
    let wanted = name.trim().to_ascii_lowercase();
    let Some(value) = compression_value(&wanted) else {
        return Err(ScanError::Failed(format!("compression {wanted} is not known")));
    };
    require_choice(props, WIA_IPA_COMPRESSION, value, "compression")?;
    write_i4(props, WIA_IPA_COMPRESSION, value)
}

fn compression_value(name: &str) -> Option<i32> {
    let code = match Compression::parse(name)? {
        Compression::None => WIA_COMPRESSION_NONE,
        Compression::Rle4 => WIA_COMPRESSION_BI_RLE4,
        Compression::Rle8 => WIA_COMPRESSION_BI_RLE8,
        Compression::G3 => WIA_COMPRESSION_G3,
        Compression::G4 => WIA_COMPRESSION_G4,
        Compression::Jpeg => WIA_COMPRESSION_JPEG,
        Compression::Jbig => WIA_COMPRESSION_JBIG,
        Compression::Jpeg2k => WIA_COMPRESSION_JPEG2K,
        Compression::Png => WIA_COMPRESSION_PNG,
    };
    i32::try_from(code).ok()
}

fn write_flag_bool(props: &IWiaPropertyStorage, id: u32, value: Option<bool>, label: &str) -> Result<(), ScanError> {
    let Some(on) = value else {
        return Ok(());
    };
    let coded = i32::from(on);
    require_choice(props, id, coded, label)?;
    write_i4(props, id, coded)
}

fn write_format(props: &IWiaPropertyStorage, format: &str) -> Result<(), ScanError> {
    let wanted = format.trim().trim_start_matches('.').to_ascii_lowercase();
    let Some(guid) = format_guid(&wanted) else {
        return Err(ScanError::Failed(format!("format {wanted} is not known")));
    };
    let offered = guid_list(props, WIA_IPA_FORMAT);
    if !offered.is_empty() && !offered.iter().any(|id| id == &guid) {
        let shown = offered.iter().filter_map(|id| format_name(*id)).collect::<Vec<_>>().join(", ");
        return Err(ScanError::Failed(format!("format {wanted} is not offered ({shown})")));
    }
    write_clsid(props, WIA_IPA_FORMAT, guid)
}

fn format_guid(name: &str) -> Option<GUID> {
    Some(guid_of(ImageFormat::parse(name)?))
}

fn guid_of(format: ImageFormat) -> GUID {
    match format {
        ImageFormat::Bmp => WiaImgFmt_BMP,
        ImageFormat::Jpeg => WiaImgFmt_JPEG,
        ImageFormat::Png => WiaImgFmt_PNG,
        ImageFormat::Tiff => WiaImgFmt_TIFF,
        ImageFormat::Jpeg2k => WiaImgFmt_JPEG2K,
    }
}

fn format_name(guid: GUID) -> Option<&'static str> {
    ImageFormat::ALL.iter().find(|format| guid_of(**format) == guid).map(|format| format.as_str())
}

fn compression_name(value: i32) -> Option<&'static str> {
    Compression::ALL.iter().find(|name| compression_value(name.as_str()) == Some(value)).map(|name| name.as_str())
}

fn choice_list(props: &IWiaPropertyStorage, id: u32) -> Vec<i32> {
    let Ok((flags, raw)) = property_numbers(props, id) else {
        return Vec::new();
    };
    if flags & WIA_PROP_LIST == 0 { Vec::new() } else { listed_values(&raw) }
}

fn area_of(props: &IWiaPropertyStorage, x_id: u32, y_id: u32) -> Option<(u32, u32)> {
    positive_pair(read_i4(props, x_id).ok(), read_i4(props, y_id).ok())
}

/// Both sides must be present and positive.
fn positive_pair(x: Option<i32>, y: Option<i32>) -> Option<(u32, u32)> {
    Some((positive_u32(x)?, positive_u32(y)?))
}

/// The scannable area of the glass in thousandths of an inch. WIA 2.0 publishes it on the flatbed
/// item (`WIA_IPS_MAX_*_SIZE`); a WIA 1.0 style root publishes `WIA_DPS_*_BED_SIZE`.
fn bed_area(item: &IWiaPropertyStorage, root: &IWiaPropertyStorage) -> Option<(u32, u32)> {
    area_of(item, WIA_IPS_MAX_HORIZONTAL_SIZE, WIA_IPS_MAX_VERTICAL_SIZE)
        .or_else(|| area_of(root, WIA_DPS_HORIZONTAL_BED_SIZE, WIA_DPS_VERTICAL_BED_SIZE))
}

/// The scannable area of a feeder sheet; same sources as [`bed_area`].
fn sheet_area(item: &IWiaPropertyStorage, root: &IWiaPropertyStorage) -> Option<(u32, u32)> {
    area_of(item, WIA_IPS_MAX_HORIZONTAL_SIZE, WIA_IPS_MAX_VERTICAL_SIZE)
        .or_else(|| area_of(root, WIA_DPS_HORIZONTAL_SHEET_FEED_SIZE, WIA_DPS_VERTICAL_SHEET_FEED_SIZE))
}

/// Write a pixel position or extent. A published range's step is honored; otherwise the value is written as given.
fn write_stepped(props: &IWiaPropertyStorage, id: u32, value: u32, positive: bool, soft: bool) -> Result<u32, ScanError> {
    let mut number = as_i32(value)?;
    if let Ok((flags, raw)) = property_numbers(props, id)
        && flags & WIA_PROP_RANGE != 0
        && let Some(span) = spanned_values(&raw)
    {
        number = align_to_span(number, span, positive)?;
    } else if positive && number <= 0 {
        return Err(ScanError::Failed("the scan region has no size".into()));
    }
    if soft {
        write_i4_soft(props, id, number)?;
    } else {
        write_i4(props, id, number)?;
    }
    u32::try_from(number).map_err(|_| ScanError::Failed("a scanner value does not fit".into()))
}

fn choice_span(props: &IWiaPropertyStorage, id: u32) -> Option<ValueSpan> {
    let (flags, raw) = property_numbers(props, id).ok()?;
    if flags & WIA_PROP_RANGE == 0 { None } else { spanned_values(&raw) }
}

fn require_choice(props: &IWiaPropertyStorage, id: u32, value: i32, label: &str) -> Result<(), ScanError> {
    let Ok((flags, raw)) = property_numbers(props, id) else {
        return Ok(());
    };
    if flags & WIA_PROP_LIST != 0 {
        let values = listed_values(&raw);
        if !values.is_empty() && !values.contains(&value) {
            let shown = values.iter().map(i32::to_string).collect::<Vec<_>>().join(", ");
            return Err(ScanError::Failed(format!("{label} {value} is not offered ({shown})")));
        }
    }
    if flags & WIA_PROP_FLAG != 0 {
        let mask = raw.last().copied().unwrap_or(0);
        if mask != 0 && value != 0 && value & mask != value {
            return Err(ScanError::Failed(format!("{label} {value} is not offered (mask {mask})")));
        }
    }
    if flags & WIA_PROP_RANGE != 0
        && let Some(span) = spanned_values(&raw)
        && !span.contains(value)
    {
        return Err(ScanError::Failed(format!("{label} {value} is outside {} to {}, step {}", span.min, span.max, span.step)));
    }
    Ok(())
}

fn require_color(props: &IWiaPropertyStorage, color: Color) -> Result<(), ScanError> {
    let Ok((flags, raw)) = property_numbers(props, WIA_IPA_DATATYPE) else {
        return Ok(());
    };
    if flags & WIA_PROP_LIST == 0 {
        return Ok(());
    }
    let values = listed_values(&raw);
    if values.is_empty() || values.contains(&color.data_type()) {
        return Ok(());
    }
    let names = values.iter().filter_map(|value| Color::from_data_type(*value)).map(Color::name).collect::<Vec<_>>().join(", ");
    Err(ScanError::Failed(format!("color {} is not offered ({names})", color.name())))
}

fn write_optional(props: &IWiaPropertyStorage, id: u32, value: Option<i32>, label: &str) -> Result<(), ScanError> {
    let Some(value) = value else {
        return Ok(());
    };
    require_choice(props, id, value, label)?;
    write_i4(props, id, value)
}

fn property_numbers(props: &IWiaPropertyStorage, id: u32) -> Result<(u32, Vec<i32>), ScanError> {
    let attributes = ffi::property_attributes(props, id);
    attributes.status.map_err(failed)?;
    Ok((attributes.flags, ffi::variant_i32_vector(&attributes.valid)))
}

fn properties(item: &IWiaItem2) -> Result<IWiaPropertyStorage, ScanError> {
    item.cast().map_err(failed)
}

fn read_i4(props: &IWiaPropertyStorage, id: u32) -> Result<i32, ScanError> {
    i4_of(&ffi::read_property(props, id).map_err(failed)?)
}

/// Skip a write the driver rejects when the property already holds `value`.
/// The inbox WSD driver publishes resolution as read-only after it has validated the ticket.
/// Best-effort write for inbox WSD items that reject many IPS properties after ticket validation.
fn write_i4_soft(props: &IWiaPropertyStorage, id: u32, value: i32) -> Result<(), ScanError> {
    if write_i4(props, id, value).is_err() {
        log::debug!("scan: left property {id:#x} at the driver value (wanted {value})");
    }
    Ok(())
}

fn write_i4_if_needed(props: &IWiaPropertyStorage, id: u32, value: i32, soft: bool) -> Result<(), ScanError> {
    match read_i4(props, id) {
        Ok(current) if current == value => Ok(()),
        Ok(_) if soft => write_i4_soft(props, id, value),
        Ok(_) => write_i4(props, id, value),
        Err(_) if soft => write_i4_soft(props, id, value),
        Err(_) => match write_i4(props, id, value) {
            Ok(()) => Ok(()),
            // The inbox WSD item omits resolution and color; the validated ticket already has them.
            Err(_) => {
                log::debug!("scan: property {id:#x} is not on this item; leaving it");
                Ok(())
            }
        },
    }
}

fn write_i4(props: &IWiaPropertyStorage, id: u32, value: i32) -> Result<(), ScanError> {
    ffi::write_i32_property(props, id, value).map_err(|e| {
        let text = describe_error(&e);
        let code = e.code().0 as u32;
        if std::env::var_os("PDFCRAFT_SCAN_WRITE_LOG").is_some() {
            eprintln!("scan: write_i4 property {id} ({id:#x}) = {value} failed: {text} ({code:#010x})");
        }
        log::warn!("scan: property {id:#x} = {value} was not accepted: {text} ({code:#010x})");
        if pdfcraft_scan::wia_error_message(code).is_some() { ScanError::from_hresult(code, &e.message()) } else { property_error(id, code, text) }
    })
}

/// A property the driver refused with a code the engine has no sentence for: the sentence names the
/// property; the code and its cause stay.
fn property_error(id: u32, code: u32, text: String) -> ScanError {
    match ScanError::from_hresult(code, &text) {
        ScanError::Wia { cause, .. } => ScanError::Wia { cause, message: format!("property {id}: {text}") },
        other => other,
    }
}

fn write_clsid(props: &IWiaPropertyStorage, id: u32, guid: GUID) -> Result<(), ScanError> {
    ffi::write_guid_property(props, id, guid).map_err(|e| {
        let text = describe_error(&e);
        let code = e.code().0 as u32;
        if pdfcraft_scan::wia_error_message(code).is_some() { ScanError::from_hresult(code, &e.message()) } else { property_error(id, code, text) }
    })
}

fn read_guid(props: &IWiaPropertyStorage, id: u32) -> Option<GUID> {
    ffi::variant_guid(&ffi::read_property(props, id).ok()?)
}

fn guid_list(props: &IWiaPropertyStorage, id: u32) -> Vec<GUID> {
    let attributes = ffi::property_attributes(props, id);
    if attributes.status.is_ok() && attributes.flags & WIA_PROP_LIST != 0 { ffi::variant_guid_vector(&attributes.valid) } else { Vec::new() }
}

fn dump_properties(props: &IWiaPropertyStorage) -> Vec<PropertyInfo> {
    let mut probed = Vec::new();
    for id in [
        WIA_IPA_ITEM_NAME,
        WIA_IPA_ITEM_FLAGS,
        WIA_IPA_FORMAT,
        WIA_IPA_ITEM_CATEGORY,
        WIA_IPS_XRES,
        WIA_IPS_YRES,
        WIA_IPS_XPOS,
        WIA_IPS_YPOS,
        WIA_IPS_XEXTENT,
        WIA_IPS_YEXTENT,
    ] {
        if read_i4(props, id).is_ok() || read_string(props, id).is_ok() {
            let (flags, access, attrs) = dump_attributes(props, id);
            let value = dump_value(props, id, 0);
            let value = if attrs.is_empty() { value } else { format!("{value}; attrs={attrs}") };
            probed.push(PropertyInfo { id, name: String::new(), vt: 0, flags, value, access });
        }
    }
    let Some(entries) = ffi::enumerate_properties(props) else {
        probed.sort_by_key(|prop| prop.id);
        return probed;
    };
    let mut out = Vec::new();
    // The walk is bounded: a bad enumerator must not run forever.
    for entry in entries {
        let (id, vt) = (entry.id, entry.vt);
        let (flags, access, attrs) = dump_attributes(props, id);
        let value = dump_value(props, id, vt);
        let value = if attrs.is_empty() { value } else { format!("{value}; attrs={attrs}") };
        out.push(PropertyInfo { id, name: entry.name, vt, flags, value, access });
    }
    if out.is_empty() {
        out = probed;
    }
    out.sort_by_key(|prop| prop.id);
    out
}

fn dump_attributes(props: &IWiaPropertyStorage, id: u32) -> (u32, String, String) {
    let attributes = ffi::property_attributes(props, id);
    let access = access_label(attributes.flags);
    let attrs = if attributes.status.is_ok() {
        let numbers = ffi::variant_i32_vector(&attributes.valid);
        let guids = ffi::variant_guid_vector(&attributes.valid);
        if !guids.is_empty() {
            guids.iter().map(|g| format_name(*g).map_or_else(|| format!("{g:?}"), str::to_string)).collect::<Vec<_>>().join(",")
        } else if !numbers.is_empty() {
            numbers.iter().map(i32::to_string).collect::<Vec<_>>().join(",")
        } else {
            String::new()
        }
    } else {
        String::new()
    };
    (attributes.flags, access, attrs)
}

fn access_label(flags: u32) -> String {
    let read = flags & WIA_PROP_READ != 0;
    let write = flags & WIA_PROP_WRITE != 0;
    match (read, write) {
        (true, true) => "rw".into(),
        (true, false) => "r".into(),
        (false, true) => "w".into(),
        _ => "none".into(),
    }
}

fn dump_value(props: &IWiaPropertyStorage, id: u32, vt: u16) -> String {
    if vt == VT_I4.0 || vt == VT_UI4.0 || vt == VT_UI2.0 {
        return read_i4(props, id).map(|n| n.to_string()).unwrap_or_else(|_| "—".into());
    }
    if vt == VT_BSTR.0 || vt == VT_LPWSTR.0 {
        return read_string(props, id).unwrap_or_else(|_| "—".into());
    }
    if vt == VT_CLSID.0 {
        return read_guid(props, id).map(|g| format_name(g).map_or_else(|| format!("{g:?}"), str::to_string)).unwrap_or_else(|| "—".into());
    }
    match ffi::read_property(props, id) {
        Ok(value) => format!("vt={}", value.vt().0),
        Err(_) => "—".into(),
    }
}

fn read_string(props: &IWiaPropertyStorage, id: u32) -> Result<String, ScanError> {
    string_of(&ffi::read_property(props, id).map_err(failed)?)
}

fn i4_of(value: &PROPVARIANT) -> Result<i32, ScanError> {
    let Some(wide) = ffi::variant_integer(value) else {
        return Err(ScanError::Failed(format!("a scanner property was type {}, not an integer", value.vt().0)));
    };
    i32::try_from(wide).map_err(|_| ScanError::Failed("a scanner value does not fit".into()))
}

fn string_of(value: &PROPVARIANT) -> Result<String, ScanError> {
    match ffi::variant_text(value) {
        Some(text) => text.map_err(ScanError::Failed),
        None => Err(ScanError::Failed(format!("a scanner name was type {}", value.vt().0))),
    }
}

/// `IWiaDataTransfer::idtGetBandedData`. The driver calls back with one band at a time.
#[allow(clippy::too_many_arguments)]
fn receive_bands(
    data: &IWiaDataTransfer,
    width: u32,
    height: u32,
    depth: u32,
    rule: pdfcraft_scan::bw::PageRule,
    limit: usize,
    pages_requested: usize,
    progress: Option<pdfcraft_scan::ProgressToken>,
    root: &IWiaPropertyStorage,
    feeder: bool,
) -> Result<Vec<Page>, ScanError> {
    let expected_bytes = pdfcraft_scan::estimated_bmp_bytes(width, height, depth);
    if let Some(progress) = &progress {
        progress.set_expected_bytes(expected_bytes);
    }
    let state = Rc::new(RefCell::new(Bands { file: None, path: None, pages: Vec::new(), limit, rule, error: None, progress, expected_bytes }));
    let sink = band_callback(&state);
    let transfer = ffi::banded_transfer(data, band_bytes(width, depth), &sink);
    let mut guard = state.try_borrow_mut().map_err(|_| ScanError::Failed("the band state was still borrowed".into()))?;
    if let Err(error) = finish_band(&mut guard) {
        return Err(ScanError::Failed(error));
    }
    if let Some(error) = guard.error.take() {
        return Err(ScanError::Failed(error));
    }
    let pages = std::mem::take(&mut guard.pages);
    drop(guard);
    let status_after = HandlingStatus::from_bits(read_i4(root, WIA_DPS_DOCUMENT_HANDLING_STATUS).unwrap_or(0));
    let page_count = pages.len();
    let download_error =
        transfer.err().map(failed).map(|error| pdfcraft_scan::explain_transfer_failure(error, status_after, feeder, page_count, pages_requested));
    // A jam with pages already in hand stays an error. An empty feeder after those pages does not.
    if let Err(error) = pdfcraft_scan::after_transfer_checked(pages_requested, feeder, page_count, false, download_error) {
        if pages.is_empty() {
            return Err(error);
        }
        let acquired = Acquired { source: if feeder { Source::Feeder } else { Source::Glass }, pages: pages.into_iter().take(limit).collect() };
        return Err(ScanError::Incomplete { message: error.to_string(), cause: error.cause(), acquired });
    }
    Ok(pages.into_iter().take(limit).collect())
}

fn flag(value: i32, bit: u32) -> bool {
    u32::try_from(value).unwrap_or(0) & bit != 0
}

fn as_i32(value: u32) -> Result<i32, ScanError> {
    i32::try_from(value).map_err(|_| ScanError::Failed("a scanner value does not fit".into()))
}

#[track_caller]
fn failed(error: impl Into<windows::core::Error>) -> ScanError {
    let error = error.into();
    let code = error.code().0 as u32;
    let text = describe_error(&error);
    let loc = std::panic::Location::caller();
    // The dialog shows `text` only; the HRESULT and call site stay on this log line.
    log::warn!("scan: {text} ({code:#010x}) at {loc}");
    WIA_FAILURE_LOGGED.set(true);
    ScanError::from_hresult(code, &error.message())
}

fn describe_error(error: &windows::core::Error) -> String {
    describe_windows_error(error.code().0 as u32, &error.message())
}

/// Puts the media type back when `acquire` returns, including on an error.
struct TymedRestore<'a> {
    item: &'a IWiaPropertyStorage,
    value: i32,
}

impl Drop for TymedRestore<'_> {
    fn drop(&mut self) {
        let _ = write_i4(self.item, WIA_IPA_TYMED, self.value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_band_is_at_least_sixty_four_kilobytes() {
        assert!(band_bytes(100, 8) >= 64 * 1024);
        assert!(band_bytes(8500, 24) > 64 * 1024);
    }

    #[test]
    fn compression_and_format_names_round_trip() {
        for name in ["none", "g4", "jpeg", "jpg", "png", "jpeg2k", "jp2"] {
            let code = compression_value(name).expect(name);
            let shown = compression_name(code).expect(name);
            assert!(shown == name || (name == "jpg" && shown == "jpeg") || (name == "jp2" && shown == "jpeg2k"), "{name} -> {shown}");
        }
        assert_eq!(compression_value("bogus"), None);
        assert_eq!(format_name(format_guid("bmp").expect("bmp")), Some("bmp"));
        assert_eq!(format_name(format_guid("jpg").expect("jpg")), Some("jpeg"));
        assert_eq!(format_guid("bmp"), Some(WiaImgFmt_BMP));
        assert_eq!(format_guid("jpeg"), Some(WiaImgFmt_JPEG));
        assert_eq!(format_guid("gif"), None);
    }

    #[test]
    fn source_matches_by_category_or_item_name() {
        assert!(source_matches(Source::Feeder, Some(&WIA_CATEGORY_FEEDER), ""));
        assert!(source_matches(Source::Feeder, None, "Feeder"));
        assert!(source_matches(Source::Glass, Some(&WIA_CATEGORY_FLATBED), ""));
        assert!(source_matches(Source::Glass, None, "flatbed"));
        assert!(source_matches(Source::Glass, None, "Glass"));
        assert!(!source_matches(Source::Feeder, Some(&WIA_CATEGORY_FLATBED), "flatbed"));
        assert!(!source_matches(Source::Auto, Some(&WIA_CATEGORY_FEEDER), "feeder"));
    }

    #[test]
    fn handling_select_value_requests_feeder_duplex_bits() {
        assert_eq!(handling_select_value(Source::Feeder, false).expect("feeder"), as_i32(FEEDER).expect("FEEDER"));
        assert_eq!(handling_select_value(Source::Feeder, true).expect("duplex"), as_i32(FEEDER | DUPLEX).expect("FEEDER|DUPLEX"));
        assert_eq!(handling_select_value(Source::Glass, false).expect("glass"), as_i32(FLATBED).expect("FLATBED"));
        assert_eq!(handling_select_value(Source::Glass, true).expect("glass duplex"), as_i32(FLATBED | DUPLEX).expect("FLATBED|DUPLEX"));
        assert!(handling_select_value(Source::Auto, false).is_err());
    }

    #[test]
    fn a_flag_mask_lists_only_the_intents_the_driver_allows() {
        // Typical intent flag mask: color|gray|text|min-size|max-quality|best-preview bits.
        let mask = Intent::Color.driver()
            | Intent::Gray.driver()
            | Intent::Text.driver()
            | Intent::MinimizeSize.driver()
            | Intent::MaximizeQuality.driver()
            | Intent::BestPreview.driver();
        let offered = intents_from_flag_mask(mask);
        assert_eq!(offered, vec![Intent::Color, Intent::Gray, Intent::Text, Intent::MinimizeSize, Intent::MaximizeQuality, Intent::BestPreview,]);
        assert_eq!(intents_from_flag_mask(Intent::Text.driver()), vec![Intent::Text]);
        assert!(intents_from_flag_mask(0).is_empty());
    }

    #[test]
    fn a_device_that_fails_to_open_is_left_out_and_the_rest_stay() {
        let mut listed = Vec::new();
        let first = keep_opened(&mut listed, "glass-1", "Glass", Ok("glass"));
        let offline = keep_opened(
            &mut listed,
            "usb-2",
            "USB scanner",
            Err(ScanError::Failed("The scanner is offline. Check that it is powered on and connected.".into())),
        );
        let unnamed = keep_opened(&mut listed, "dev-2", "", Err(ScanError::Failed("CreateDevice failed".into())));
        let wsd = keep_opened(&mut listed, "wsd-1", "Network scanner (WSD)", Ok("wsd"));
        assert_eq!(listed, vec!["glass", "wsd"]);
        assert!(first.is_none());
        assert!(wsd.is_none());
        let offline = offline.unwrap_or_default();
        assert!(offline.contains("USB scanner"), "{offline}");
        assert!(offline.contains("usb-2"), "{offline}");
        assert!(offline.contains("offline"), "{offline}");
        let unnamed = unnamed.unwrap_or_default();
        assert!(unnamed.contains("dev-2"), "{unnamed}");
        assert!(unnamed.contains("CreateDevice failed"), "{unnamed}");
        assert!(!unnamed.contains("()"), "{unnamed}");
    }

    #[test]
    fn integer_properties_read_whatever_width_the_driver_uses() {
        assert_eq!(i4_of(&PROPVARIANT::from(300i32)).expect("i4"), 300);
        assert_eq!(i4_of(&PROPVARIANT::from(-5i32)).expect("negative"), -5);
        assert_eq!(i4_of(&PROPVARIANT::from(8500u32)).expect("ui4"), 8500);
        assert!(i4_of(&PROPVARIANT::from(u32::MAX)).is_err(), "a value past i32 is refused, not wrapped");
        assert_eq!(i4_of(&PROPVARIANT::from(600u16)).expect("ui2"), 600);
        assert_eq!(i4_of(&PROPVARIANT::from(-7i16)).expect("i2"), -7);
        let empty = PROPVARIANT::default();
        assert!(i4_of(&empty).is_err(), "a missing property (VT_EMPTY) is an error");
    }

    #[test]
    fn text_properties_read_bstr_and_refuse_other_types() {
        assert_eq!(string_of(&PROPVARIANT::from("Scanner")).expect("bstr"), "Scanner");
        assert!(string_of(&PROPVARIANT::from(3i32)).is_err());
    }

    #[test]
    fn a_scan_area_needs_both_sides_positive() {
        assert_eq!(positive_pair(Some(8500), Some(11700)), Some((8500, 11700)));
        assert_eq!(positive_pair(Some(8500), Some(0)), None);
        assert_eq!(positive_pair(None, Some(11700)), None);
        assert_eq!(positive_pair(Some(-1), Some(5)), None);
        assert_eq!(positive_u32(Some(300)), Some(300));
        assert_eq!(positive_u32(Some(0)), None);
    }
}
