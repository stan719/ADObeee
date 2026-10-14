//! Impresión en Windows sin instalar nada: el PDF se rasteriza con el renderizador que trae el
//! propio Windows 10/11 (`Windows.Data.Pdf`) y cada página se manda al spooler por GDI, con
//! copias, dúplex, color y papel en el `DEVMODE` de la impresora. Sirve para impresoras físicas
//! y virtuales (las virtuales, como "Microsoft Print to PDF", pueden pedir dónde guardar).
//!
//! Este crate existe aparte porque la API de impresión de Windows exige `unsafe`, y el crate
//! `pdfcraft-print` lo prohíbe: el `unsafe` queda confinado acá.

#![cfg(windows)]

/// Error de impresión, con el mensaje listo para mostrar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    Spool(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Spool(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for Error {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Printer {
    pub name: String,
    pub default: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Duplex {
    #[default]
    Off,
    LongEdge,
    ShortEdge,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Job {
    /// `None` = la impresora predeterminada del sistema.
    pub printer: Option<String>,
    pub copies: u32,
    pub collate: bool,
    pub duplex: Duplex,
    pub grayscale: bool,
    pub title: String,
}

use windows::{
    core::{w, HSTRING, PCWSTR},
    Data::Pdf::{PdfDocument, PdfPage, PdfPageRenderOptions},
    Graphics::Imaging::{
        BitmapAlphaMode, BitmapDecoder, BitmapPixelFormat, BitmapTransform, ColorManagementMode,
        ExifOrientationMode,
    },
    Storage::Streams::{DataWriter, InMemoryRandomAccessStream},
    UI::Color,
    Win32::{
        Foundation::{HANDLE, HWND},
        Graphics::{
            Gdi::{
                CreateDCW, DeleteDC, GetDeviceCaps, ResetDCW, SetStretchBltMode, StretchDIBits, BITMAPINFO,
                BITMAPINFOHEADER, DEVMODEW, DIB_RGB_COLORS, DMCOLLATE_FALSE, DMCOLLATE_TRUE, DMCOLOR_COLOR,
                DMCOLOR_MONOCHROME, DMDUP_HORIZONTAL, DMDUP_SIMPLEX, DMDUP_VERTICAL, DM_COLLATE, DM_COLOR,
                DM_COPIES, DM_DUPLEX, DM_ORIENTATION, DM_PAPERLENGTH, DM_PAPERSIZE, DM_PAPERWIDTH, HALFTONE,
                HDC, LOGPIXELSX, PHYSICALHEIGHT, PHYSICALOFFSETX, PHYSICALOFFSETY, PHYSICALWIDTH, SRCCOPY,
            },
            Printing::{ClosePrinter, DocumentPropertiesW, EnumPrintersW, OpenPrinterW, PRINTER_INFO_2W, PRINTER_INFO_4W},
        },
        Storage::Xps::{AbortDoc, EndDoc, EndPage, StartDocW, StartPage, DOCINFOW},
    },
};

/// `PRINTER_ENUM_LOCAL | PRINTER_ENUM_CONNECTIONS`: impresoras locales (físicas y virtuales)
/// y las de red que el usuario tiene conectadas.
const ENUM_LOCAL_AND_CONNECTIONS: u32 = 0x2 | 0x4;
/// Resolución máxima a la que se rasteriza (una A4 a 600 dpi serían ~140 MB por página).
const MAX_DPI: f32 = 300.0;
/// Lado máximo, en píxeles, de la imagen de una página.
const MAX_SIDE_PX: f32 = 9000.0;
/// `DMDUP_VERTICAL` / `DMDUP_HORIZONTAL` se definen respecto de la hoja en vertical. Si en una
/// hoja apaisada el dúplex sale al revés, poner esto en `false`.
const SWAP_DUPLEX_ON_LANDSCAPE: bool = true;
/// Si la impresora figura sin conexión en Windows, no se manda el trabajo y se avisa enseguida.
/// En `false` el trabajo queda en la cola y sale cuando la impresora vuelva (como en otros programas).
const REFUSE_WHEN_OFFLINE: bool = true;

#[link(name = "winspool")]
unsafe extern "system" {
    fn GetDefaultPrinterW(buffer: *mut u16, size: *mut u32) -> i32;
}

#[link(name = "advapi32")]
unsafe extern "system" {
    fn RegGetValueW(
        hkey: isize,
        subkey: *const u16,
        value: *const u16,
        flags: u32,
        kind: *mut u32,
        data: *mut core::ffi::c_void,
        len: *mut u32,
    ) -> i32;
}

const HKEY_CURRENT_USER: isize = 0x8000_0001_u32 as i32 as isize;
const HKEY_LOCAL_MACHINE: isize = 0x8000_0002_u32 as i32 as isize;
/// `RRF_RT_REG_BINARY`: solo valores binarios.
const RRF_RT_REG_BINARY: u32 = 0x8;

fn werr(e: windows::core::Error) -> Error {
    Error::Spool(format!("error de impresión de Windows: {e}"))
}

/// Buffer alineado a 8 bytes: las estructuras de Win32 que se leen de él llevan punteros.
struct Aligned(Vec<u64>);

impl Aligned {
    fn new(bytes: usize) -> Self {
        Aligned(vec![0u64; bytes.div_ceil(8).max(1)])
    }
    fn bytes(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.0.as_mut_ptr().cast::<u8>(), self.0.len() * 8) }
    }
    fn ptr<T>(&mut self) -> *mut T {
        self.0.as_mut_ptr().cast()
    }
}

struct Dc(HDC);
impl Drop for Dc {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteDC(self.0);
        }
    }
}

struct PrinterHandle(HANDLE);
impl Drop for PrinterHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = ClosePrinter(self.0);
        }
    }
}

/// El nombre de la impresora predeterminada del usuario.
fn default_printer() -> Option<String> {
    let mut len = 0u32;
    unsafe { GetDefaultPrinterW(std::ptr::null_mut(), &mut len) };
    if len == 0 {
        return None;
    }
    let mut buf = vec![0u16; len as usize];
    if unsafe { GetDefaultPrinterW(buf.as_mut_ptr(), &mut len) } == 0 {
        return None;
    }
    let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..n]))
}

/// Las impresoras que conoce Windows, físicas y virtuales (vacío si no hay ninguna).
pub fn printers() -> Vec<Printer> {
    let default = default_printer();
    let (mut needed, mut returned) = (0u32, 0u32);
    unsafe {
        // La primera llamada solo pide el tamaño: falla con ERROR_INSUFFICIENT_BUFFER.
        let _ = EnumPrintersW(ENUM_LOCAL_AND_CONNECTIONS, PCWSTR::null(), 4, None, &mut needed, &mut returned);
        if needed == 0 {
            return Vec::new();
        }
        let mut buf = Aligned::new(needed as usize);
        let size = (buf.0.len() * 8) as u32;
        needed = size;
        if EnumPrintersW(ENUM_LOCAL_AND_CONNECTIONS, PCWSTR::null(), 4, Some(buf.bytes()), &mut needed, &mut returned)
            .is_err()
        {
            return Vec::new();
        }
        let infos = std::slice::from_raw_parts(buf.ptr::<PRINTER_INFO_4W>() as *const PRINTER_INFO_4W, returned as usize);
        infos
            .iter()
            .filter_map(|i| i.pPrinterName.to_string().ok())
            .filter(|n| !n.is_empty())
            .map(|name| Printer { default: default.as_deref() == Some(name.as_str()), name })
            .collect()
    }
}

/// Si Windows da la impresora por sin conexión. Se lee el estado que ya tiene el spooler: no se
/// le pregunta a la impresora, así que responde enseguida aunque esté apagada.
fn printer_offline(printer: &str) -> bool {
    const ATTRIBUTE_WORK_OFFLINE: u32 = 0x400;
    const STATUS_OFFLINE: u32 = 0x80;
    const STATUS_NOT_AVAILABLE: u32 = 0x1000;
    const STATUS_PORT_UNAVAILABLE: u32 = 0x20_0000;
    let (mut needed, mut returned) = (0u32, 0u32);
    unsafe {
        let _ = EnumPrintersW(ENUM_LOCAL_AND_CONNECTIONS, PCWSTR::null(), 2, None, &mut needed, &mut returned);
        if needed == 0 {
            return false;
        }
        let mut buf = Aligned::new(needed as usize);
        needed = (buf.0.len() * 8) as u32;
        if EnumPrintersW(ENUM_LOCAL_AND_CONNECTIONS, PCWSTR::null(), 2, Some(buf.bytes()), &mut needed, &mut returned)
            .is_err()
        {
            return false;
        }
        let infos = std::slice::from_raw_parts(buf.ptr::<PRINTER_INFO_2W>() as *const PRINTER_INFO_2W, returned as usize);
        infos
            .iter()
            .find(|i| i.pPrinterName.to_string().ok().as_deref() == Some(printer))
            .is_some_and(|i| {
                i.Attributes & ATTRIBUTE_WORK_OFFLINE != 0
                    || i.Status & (STATUS_OFFLINE | STATUS_NOT_AVAILABLE | STATUS_PORT_UNAVAILABLE) != 0
            })
    }
}

/// Papel predeterminado de una impresora, con la hoja en vertical (ancho <= alto).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Paper {
    pub width_mm: f32,
    pub height_mm: f32,
}

/// (código `DMPAPER_*`, lado corto mm, lado largo mm) de los papeles que puede traer una impresora.
const KNOWN_PAPERS: [(i16, f32, f32); 12] = [
    (1, 215.9, 279.4),  // Carta
    (3, 279.4, 431.8),  // Tabloide
    (5, 215.9, 355.6),  // Legal
    (6, 139.7, 215.9),  // Statement
    (7, 184.2, 266.7),  // Executive
    (8, 297.0, 420.0),  // A3
    (9, 210.0, 297.0),  // A4
    (11, 148.0, 210.0), // A5
    (12, 250.0, 353.0), // B4
    (13, 176.0, 250.0), // B5
    (14, 215.9, 330.2), // Folio
    (70, 105.0, 148.0), // A6
];

/// El papel a partir de los campos de un `DEVMODE` (`dmFields`, `dmPaperSize` y medidas en décimas de mm).
fn paper_from_fields(fields: u32, code: i16, w10: i16, l10: i16) -> Option<Paper> {
    const DMF_PAPERSIZE: u32 = 0x2;
    const DMF_PAPERLENGTH: u32 = 0x4;
    const DMF_PAPERWIDTH: u32 = 0x8;
    if fields & DMF_PAPERSIZE != 0 {
        if let Some(&(_, s, l)) = KNOWN_PAPERS.iter().find(|(c, ..)| *c == code) {
            return Some(Paper { width_mm: s, height_mm: l });
        }
    }
    // Papel propio, o uno que no está en la tabla pero con medidas informadas.
    if fields & DMF_PAPERWIDTH != 0 && fields & DMF_PAPERLENGTH != 0 && w10 > 0 && l10 > 0 {
        let (a, b) = (f32::from(w10) / 10.0, f32::from(l10) / 10.0);
        return Some(Paper { width_mm: a.min(b), height_mm: a.max(b) });
    }
    None
}

/// Lee un valor binario del registro.
fn reg_binary(root: isize, subkey: &str, value: &str) -> Option<Vec<u8>> {
    let (sub, val) = (HSTRING::from(subkey), HSTRING::from(value));
    let mut len = 0u32;
    let rc = unsafe {
        RegGetValueW(root, sub.as_ptr(), val.as_ptr(), RRF_RT_REG_BINARY, std::ptr::null_mut(), std::ptr::null_mut(), &mut len)
    };
    if rc != 0 || len == 0 {
        return None;
    }
    let mut buf = vec![0u8; len as usize];
    let rc = unsafe {
        RegGetValueW(root, sub.as_ptr(), val.as_ptr(), RRF_RT_REG_BINARY, std::ptr::null_mut(), buf.as_mut_ptr().cast(), &mut len)
    };
    if rc != 0 {
        return None;
    }
    buf.truncate(len as usize);
    Some(buf)
}

/// El papel de un `DEVMODEW` guardado como bytes (offsets de la parte pública de la estructura).
fn paper_from_devmode_blob(b: &[u8]) -> Option<Paper> {
    let u16_at = |o: usize| -> Option<u16> { Some(u16::from_le_bytes([*b.get(o)?, *b.get(o + 1)?])) };
    let dm_size = usize::from(u16_at(68)?); // dmSize
    if dm_size < 84 || dm_size > b.len() {
        return None;
    }
    let fields = u32::from(u16_at(72)?) | (u32::from(u16_at(74)?) << 16); // dmFields
    // dmPaperSize en 78, dmPaperLength en 80, dmPaperWidth en 82.
    paper_from_fields(fields, u16_at(78)? as i16, u16_at(82)? as i16, u16_at(80)? as i16)
}

/// El papel guardado de la impresora, leído del registro: no se comunica con la impresora, así que
/// anda con ella apagada y no hace esperar.
fn saved_paper(printer: &str) -> Option<Paper> {
    // Las impresoras de red (\\servidor\nombre) se guardan con comas en lugar de barras.
    let key = printer.replace('\\', ",");
    // Primero lo que el usuario guardó en las preferencias de la impresora; si no, el valor de la impresora.
    reg_binary(HKEY_CURRENT_USER, "Printers\\DevModePerUser", &key)
        .and_then(|b| paper_from_devmode_blob(&b))
        .or_else(|| {
            let sub = format!("SYSTEM\\CurrentControlSet\\Control\\Print\\Printers\\{key}");
            reg_binary(HKEY_LOCAL_MACHINE, &sub, "Default DevMode").and_then(|b| paper_from_devmode_blob(&b))
        })
}

/// El papel predeterminado de la impresora: el que el usuario ve en Windows, en las preferencias
/// de esa impresora. `None` si no se puede saber.
#[allow(unused_unsafe)]
pub fn default_paper(printer: &str) -> Option<Paper> {
    // Primero lo guardado en el registro: no se comunica con la impresora.
    if let Some(paper) = saved_paper(printer) {
        return Some(paper);
    }
    // Si no hay nada guardado se le pregunta al controlador, que con la impresora apagada puede
    // mostrar "Esperando conexión de impresora".
    let name = HSTRING::from(printer);
    let mut buf = printer_devmode(&name).ok()?;
    let dm = unsafe { &*buf.ptr::<DEVMODEW>() };
    let (code, w10, l10) = unsafe {
        (dm.Anonymous1.Anonymous1.dmPaperSize, dm.Anonymous1.Anonymous1.dmPaperWidth, dm.Anonymous1.Anonymous1.dmPaperLength)
    };
    paper_from_fields(dm.dmFields.0, code, w10, l10)
}

/// Papel y orientación de una página.
#[derive(Clone, Copy, PartialEq, Debug)]
struct PageFmt {
    /// `DMPAPER_*`; 256 (`DMPAPER_USER`) = tamaño propio.
    paper: i16,
    /// Lado corto y largo en décimas de milímetro (para el tamaño propio).
    short10: i16,
    long10: i16,
    landscape: bool,
}

/// (código `DMPAPER_*`, lado corto mm, lado largo mm): A3, A4, A5, Carta, Legal.
const STANDARD: [(i16, f32, f32); 5] =
    [(8, 297.0, 420.0), (9, 210.0, 297.0), (11, 148.0, 210.0), (1, 215.9, 279.4), (5, 215.9, 355.6)];

fn page_fmt(w: f32, h: f32) -> PageFmt {
    let landscape = w > h;
    // `PdfPage::Size` llega en DIPs (1/96"). Si con eso no calza ningún papel estándar se
    // prueba en puntos (1/72"); si tampoco, se pide un tamaño propio (calculado con 1/96").
    let sides = |per_inch: f32| {
        let (a, b) = (w * 25.4 / per_inch, h * 25.4 / per_inch);
        if a <= b { (a, b) } else { (b, a) }
    };
    let standard = |(s, l): (f32, f32)| {
        STANDARD.iter().find(|(_, ss, ll)| (s - ss).abs() < 2.0 && (l - ll).abs() < 2.0).map(|&(code, ss, ll)| (code, ss, ll))
    };
    let (paper, s, l) = standard(sides(96.0))
        .or_else(|| standard(sides(72.0)))
        .unwrap_or_else(|| {
            let (s, l) = sides(96.0);
            (256, s, l)
        });
    PageFmt { paper, short10: (s * 10.0).round() as i16, long10: (l * 10.0).round() as i16, landscape }
}

fn format_of(page: &PdfPage) -> Result<PageFmt, Error> {
    let s = page.Size().map_err(werr)?;
    Ok(page_fmt(s.Width, s.Height))
}

/// Pone papel, orientación y dúplex de una página en el `DEVMODE`.
#[allow(unused_unsafe)]
fn apply_format(dm: &mut DEVMODEW, f: &PageFmt, duplex: Duplex) {
    unsafe {
        dm.Anonymous1.Anonymous1.dmOrientation = if f.landscape { 2 } else { 1 };
        dm.Anonymous1.Anonymous1.dmPaperSize = f.paper;
        dm.dmFields |= DM_ORIENTATION | DM_PAPERSIZE;
        if f.paper == 256 {
            dm.Anonymous1.Anonymous1.dmPaperWidth = f.short10;
            dm.Anonymous1.Anonymous1.dmPaperLength = f.long10;
            dm.dmFields |= DM_PAPERWIDTH | DM_PAPERLENGTH;
        }
    }
    let swap = f.landscape && SWAP_DUPLEX_ON_LANDSCAPE;
    dm.dmDuplex = match (duplex, swap) {
        (Duplex::Off, _) => DMDUP_SIMPLEX,
        (Duplex::LongEdge, false) | (Duplex::ShortEdge, true) => DMDUP_VERTICAL,
        (Duplex::ShortEdge, false) | (Duplex::LongEdge, true) => DMDUP_HORIZONTAL,
    };
    dm.dmFields |= DM_DUPLEX;
}

/// El `DEVMODE` de la impresora (con sus valores por defecto del usuario) más las opciones del trabajo.
#[allow(unused_unsafe)]
fn devmode_for(name: &HSTRING, job: &Job, first: &PageFmt) -> Result<Aligned, Error> {
    let mut buf = printer_devmode(name)?;
    let dm = unsafe { &mut *buf.ptr::<DEVMODEW>() };
    unsafe {
        dm.Anonymous1.Anonymous1.dmCopies = job.copies.clamp(1, 999) as i16;
    }
    dm.dmCollate = if job.collate { DMCOLLATE_TRUE } else { DMCOLLATE_FALSE };
    dm.dmColor = if job.grayscale { DMCOLOR_MONOCHROME } else { DMCOLOR_COLOR };
    dm.dmFields |= DM_COPIES | DM_COLLATE | DM_COLOR;
    apply_format(dm, first, job.duplex);
    Ok(buf)
}

/// El `DEVMODE` con los valores por defecto de la impresora para el usuario (los de sus
/// "Preferencias de impresión").
fn printer_devmode(name: &HSTRING) -> Result<Aligned, Error> {
    let mut h = HANDLE::default();
    unsafe { OpenPrinterW(name, &mut h, None) }.map_err(werr)?;
    let h = PrinterHandle(h);
    // Sin buffer de salida, DocumentProperties devuelve el tamaño que necesita.
    let size = unsafe { DocumentPropertiesW(HWND::default(), h.0, name, None, None, 0) };
    if size <= 0 {
        return Err(Error::Spool("no se pudieron leer las propiedades de la impresora".into()));
    }
    let mut buf = Aligned::new(size as usize);
    const DM_OUT_BUFFER: u32 = 2;
    let ok = unsafe {
        DocumentPropertiesW(HWND::default(), h.0, name, Some(buf.ptr::<DEVMODEW>()), None, DM_OUT_BUFFER)
    };
    if ok != 1 {
        return Err(Error::Spool("no se pudieron leer las propiedades de la impresora".into()));
    }
    Ok(buf)
}

/// Rasteriza una página para que entre en la hoja de `hdc` sin deformarse. Devuelve los píxeles
/// BGRA, su ancho y alto, y el rectángulo (x, y, ancho, alto) donde dibujarla en el DC.
fn render(page: &PdfPage, hdc: HDC) -> Result<(windows::core::Array<u8>, i32, i32, [i32; 4]), Error> {
    let (sheet_w, sheet_h, dpi) = unsafe {
        (
            GetDeviceCaps(hdc, PHYSICALWIDTH) as f32,
            GetDeviceCaps(hdc, PHYSICALHEIGHT) as f32,
            GetDeviceCaps(hdc, LOGPIXELSX).max(1) as f32,
        )
    };
    let (off_x, off_y) = unsafe { (GetDeviceCaps(hdc, PHYSICALOFFSETX), GetDeviceCaps(hdc, PHYSICALOFFSETY)) };
    let size = page.Size().map_err(werr)?;
    if size.Width <= 0.0 || size.Height <= 0.0 {
        return Err(Error::Spool("una página del PDF no tiene tamaño".into()));
    }
    // La página, entera y centrada en la hoja.
    let aspect = size.Height / size.Width;
    let (mut fit_w, mut fit_h) = (sheet_w, sheet_w * aspect);
    if fit_h > sheet_h {
        (fit_w, fit_h) = (sheet_h / aspect, sheet_h);
    }
    let rect = [
        ((sheet_w - fit_w) / 2.0).round() as i32 - off_x,
        ((sheet_h - fit_h) / 2.0).round() as i32 - off_y,
        fit_w.round() as i32,
        fit_h.round() as i32,
    ];
    // Resolución de la imagen: la de la impresora hasta MAX_DPI, y con tope de tamaño.
    let k = (MAX_DPI / dpi).min(1.0).min(MAX_SIDE_PX / fit_w.max(fit_h));
    let (rw, rh) = ((fit_w * k).round().max(1.0) as u32, (fit_h * k).round().max(1.0) as u32);

    let opts = PdfPageRenderOptions::new().map_err(werr)?;
    opts.SetDestinationWidth(rw).map_err(werr)?;
    opts.SetDestinationHeight(rh).map_err(werr)?;
    opts.SetBackgroundColor(Color { A: 255, R: 255, G: 255, B: 255 }).map_err(werr)?;
    let out = InMemoryRandomAccessStream::new().map_err(werr)?;
    page.RenderWithOptionsToStreamAsync(&out, &opts).map_err(werr)?.get().map_err(werr)?;
    out.Seek(0).map_err(werr)?;
    let dec = BitmapDecoder::CreateAsync(&out).map_err(werr)?.get().map_err(werr)?;
    let (w, h) = (dec.PixelWidth().map_err(werr)? as i32, dec.PixelHeight().map_err(werr)? as i32);
    let px = dec
        .GetPixelDataTransformedAsync(
            BitmapPixelFormat::Bgra8,
            BitmapAlphaMode::Ignore,
            &BitmapTransform::new().map_err(werr)?,
            ExifOrientationMode::IgnoreExifOrientation,
            ColorManagementMode::DoNotColorManage,
        )
        .map_err(werr)?
        .get()
        .map_err(werr)?
        .DetachPixelData()
        .map_err(werr)?;
    Ok((px, w, h, rect))
}

/// Las páginas, una a una, dentro de un documento de GDI ya iniciado.
fn print_pages(doc: &PdfDocument, hdc: HDC, dm: &mut Aligned, job: &Job, first: PageFmt) -> Result<(), Error> {
    let mut current = first;
    for i in 0..doc.PageCount().map_err(werr)? {
        let page = doc.GetPage(i).map_err(werr)?;
        let fmt = format_of(&page)?;
        if fmt != current {
            // Otro papel u orientación: se cambia entre una página y la siguiente.
            let dm_ptr = dm.ptr::<DEVMODEW>();
            apply_format(unsafe { &mut *dm_ptr }, &fmt, job.duplex);
            if unsafe { ResetDCW(hdc, dm_ptr) }.is_invalid() {
                return Err(Error::Spool("la impresora no aceptó el cambio de papel".into()));
            }
            current = fmt;
        }
        let (px, w, h, [x, y, dw, dh]) = render(&page, hdc)?;
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h, // negativo: la imagen viene de arriba hacia abajo
                biPlanes: 1,
                biBitCount: 32,
                biCompression: 0, // BI_RGB
                ..Default::default()
            },
            ..Default::default()
        };
        unsafe {
            if StartPage(hdc) <= 0 {
                return Err(Error::Spool("no se pudo iniciar una página".into()));
            }
            SetStretchBltMode(hdc, HALFTONE);
            let drawn = StretchDIBits(hdc, x, y, dw, dh, 0, 0, w, h, Some(px.as_ptr().cast()), &bmi, DIB_RGB_COLORS, SRCCOPY);
            if EndPage(hdc) <= 0 || drawn <= 0 {
                return Err(Error::Spool(format!("no se pudo imprimir la página {}", i + 1)));
            }
        }
    }
    Ok(())
}

/// Imprime el PDF entero en `printer`; devuelve la cantidad de páginas. Se llama desde un hilo
/// propio: las operaciones de WinRT se esperan con `.get()`, que no va en el hilo de la UI.
fn print_pdf(pdf: &[u8], job: &Job, printer: &str) -> Result<u32, Error> {
    // El PDF se lee de memoria: no hay archivo temporal que otro usuario pueda leer o cambiar.
    let stream = InMemoryRandomAccessStream::new().map_err(werr)?;
    let writer = DataWriter::CreateDataWriter(&stream).map_err(werr)?;
    writer.WriteBytes(pdf).map_err(werr)?;
    writer.StoreAsync().map_err(werr)?.get().map_err(werr)?;
    writer.FlushAsync().map_err(werr)?.get().map_err(werr)?;
    writer.DetachStream().map_err(werr)?;
    stream.Seek(0).map_err(werr)?;
    let doc = PdfDocument::LoadFromStreamAsync(&stream)
        .map_err(werr)?
        .get()
        .map_err(|e| Error::Spool(format!("no se pudo leer el PDF: {e}")))?;
    if doc.IsPasswordProtected().map_err(werr)? {
        return Err(Error::Spool("el PDF tiene contraseña".into()));
    }
    let pages = doc.PageCount().map_err(werr)?;
    if pages == 0 {
        return Err(Error::Spool("el PDF no tiene páginas".into()));
    }

    let name = HSTRING::from(printer);
    let first = format_of(&doc.GetPage(0).map_err(werr)?)?;
    let mut dm = devmode_for(&name, job, &first)?;
    let hdc = Dc(unsafe { CreateDCW(w!("WINSPOOL"), &name, PCWSTR::null(), Some(dm.ptr::<DEVMODEW>() as *const DEVMODEW)) });
    if hdc.0.is_invalid() {
        return Err(Error::Spool(format!("no se pudo abrir la impresora «{printer}»")));
    }

    let title = HSTRING::from(job.title.as_str());
    let info = DOCINFOW {
        cbSize: std::mem::size_of::<DOCINFOW>() as i32,
        lpszDocName: PCWSTR(title.as_ptr()),
        ..Default::default()
    };
    if unsafe { StartDocW(hdc.0, &info) } <= 0 {
        return Err(Error::Spool("la impresora rechazó el trabajo (o se canceló)".into()));
    }
    match print_pages(&doc, hdc.0, &mut dm, job, first) {
        Ok(()) => {
            if unsafe { EndDoc(hdc.0) } <= 0 {
                return Err(Error::Spool("no se pudo terminar el trabajo de impresión".into()));
            }
            Ok(pages)
        }
        Err(e) => {
            unsafe { AbortDoc(hdc.0) };
            Err(e)
        }
    }
}

/// Manda un PDF listo para imprimir al spooler de Windows.
pub fn submit(pdf: &[u8], job: &Job) -> Result<String, Error> {
    let printer = match &job.printer {
        Some(p) => p.clone(),
        None => default_printer()
            .or_else(|| printers().into_iter().next().map(|p| p.name))
            .ok_or_else(|| Error::Spool("no hay ninguna impresora disponible".into()))?,
    };
    if REFUSE_WHEN_OFFLINE && printer_offline(&printer) {
        return Err(Error::Spool(format!(
            "La impresora «{printer}» está sin conexión. Encendela y revisá que esté conectada, o elegí otra."
        )));
    }
    let pages = std::thread::scope(|s| {
        std::thread::Builder::new()
            .name("print job".into())
            .spawn_scoped(s, || print_pdf(pdf, job, &printer))
            .map_err(|e| Error::Spool(format!("no se pudo iniciar el hilo de impresión: {e}")))
            .and_then(|h| h.join().unwrap_or_else(|_| Err(Error::Spool("falló el hilo de impresión".into()))))
    })?;
    Ok(format!("Enviado a {printer} ({pages} página(s))"))
}
