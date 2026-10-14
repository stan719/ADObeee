//! The typed document model. Numbers are OFD's own (millimetres, y down); see [`crate::geom`].
//! Resource references (`Font`, `ResourceID`, `ColorSpace`, `DrawParam`) are the IDs as written;
//! [`Resources`] resolves them. Package paths are resolved to entry names from the package root.

use std::collections::BTreeMap;

use crate::geom::{Matrix, OfdBox};

/// One OFD document: the first `DocBody` of `OFD.xml` and its `Document.xml`.
#[derive(Clone, Debug, PartialEq)]
pub struct OfdDocument {
    /// `OFD/@Version` (usually "1.0" or "1.1").
    pub version: String,
    /// `OFD/@DocType` (usually "OFD"; "OFD-A" for archival documents).
    pub doc_type: String,
    /// How many `DocBody` elements `OFD.xml` has. Only the first is read.
    pub doc_bodies: usize,
    pub info: DocInfo,
    /// Entry name of `Document.xml`.
    pub doc_root: String,
    /// Entry name of the signature list (`Signatures.xml`), if the document is signed. The
    /// signatures themselves are not read yet.
    pub signatures: Option<String>,
    /// The document's default page area (`CommonData/PageArea`).
    pub page_area: Option<PageArea>,
    pub templates: Vec<TemplatePage>,
    pub pages: Vec<Page>,
    /// Public, document and page resources merged by ID (IDs are unique in a document).
    pub resources: Resources,
    /// What was skipped or tolerated while reading, for the user or a conversion report.
    pub warnings: Vec<String>,
}

/// `DocInfo`: the document's metadata.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DocInfo {
    pub doc_id: Option<String>,
    pub title: Option<String>,
    pub author: Option<String>,
    pub subject: Option<String>,
    pub creator: Option<String>,
    pub creation_date: Option<String>,
    pub mod_date: Option<String>,
}

/// `PageArea` (`CT_PageArea`): the page boxes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PageArea {
    pub physical: OfdBox,
    pub application: Option<OfdBox>,
    pub content: Option<OfdBox>,
    pub bleed: Option<OfdBox>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ZOrder {
    Background,
    Foreground,
}

/// A template page (`CommonData/TemplatePage`): content pages draw under or over themselves.
#[derive(Clone, Debug, PartialEq)]
pub struct TemplatePage {
    pub id: u32,
    pub name: Option<String>,
    pub z_order: ZOrder,
    /// Entry name of its content file.
    pub location: String,
    pub content: PageContent,
}

/// A page (`Pages/Page`) with its content file read.
#[derive(Clone, Debug, PartialEq)]
pub struct Page {
    pub id: u32,
    /// Entry name of its content file.
    pub location: String,
    /// The page's area: its own `Area`, else the document's `PageArea`.
    pub area: PageArea,
    /// Templates drawn with this page, in order. `z_order` is the page's override, if any.
    pub templates: Vec<TemplateRef>,
    pub content: PageContent,
    /// The page's annotations (`Annotations` ▸ `PageAnnot`), in order.
    pub annotations: Vec<Annotation>,
}

/// `Annot`: an annotation (a stamp, a watermark, a link, …) with its appearance.
#[derive(Clone, Debug, PartialEq)]
pub struct Annotation {
    /// `Type`: "Link", "Path", "Highlight", "Stamp", "Watermark" or another the writer chose.
    pub kind: String,
    pub subtype: Option<String>,
    pub visible: bool,
    pub print: bool,
    /// `NoView`: shown only when printed.
    pub no_view: bool,
    /// `Appearance/@Boundary`: where the appearance sits on the page. Its objects' own
    /// coordinates start at this box's corner.
    pub boundary: Option<OfdBox>,
    /// The appearance's objects.
    pub objects: Vec<Object>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TemplateRef {
    pub template_id: u32,
    pub z_order: Option<ZOrder>,
}

/// The drawable content of a page or template page.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PageContent {
    /// The content file's own `Area`, if it has one.
    pub area: Option<PageArea>,
    pub layers: Vec<Layer>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayerKind {
    Body,
    Background,
    Foreground,
    Custom,
}

/// `Content/Layer`: objects drawn in order.
#[derive(Clone, Debug, PartialEq)]
pub struct Layer {
    pub id: Option<u32>,
    pub kind: LayerKind,
    pub draw_param: Option<u32>,
    pub objects: Vec<Object>,
}

/// A page object.
#[derive(Clone, Debug, PartialEq)]
pub enum Object {
    Path(PathObject),
    Text(TextObject),
    Image(ImageObject),
    /// A composite graphic unit placed on the page (`CompositeObject`); the unit is in
    /// [`Resources::composites`].
    Composite(CompositeObject),
    /// `PageBlock`: a group of objects.
    Block(PageBlock),
}

/// Attributes every graphic unit has (`CT_GraphicUnit`).
#[derive(Clone, Debug, PartialEq)]
pub struct Graphic {
    pub id: Option<u32>,
    /// Where the object sits on the page; its own coordinates start at this box's corner.
    pub boundary: OfdBox,
    pub ctm: Option<Matrix>,
    pub draw_param: Option<u32>,
    pub line_width: Option<f64>,
    /// 0 (transparent) to 255 (opaque).
    pub alpha: u8,
    pub visible: bool,
    /// `Clips`: the object is drawn only where all of these overlap (their intersection).
    pub clips: Vec<Clip>,
    /// `Clips/@TransFlag`: the clips are in the object's own space, so its CTM applies to them
    /// too. Otherwise (the default) only the move to the boundary's corner does.
    pub clips_follow_ctm: bool,
}

impl Graphic {
    /// The matrix from the object's own space to page space (mm): its CTM, then the move to its
    /// boundary's corner.
    pub fn to_page(&self) -> Matrix {
        self.ctm.unwrap_or(Matrix::IDENTITY).then(&Matrix::translate(self.boundary.x, self.boundary.y))
    }

    /// The matrix from one of this object's clip areas to page space (mm): the area's CTM, then
    /// the object's CTM when [`Graphic::clips_follow_ctm`], then the move to the object's
    /// boundary corner. An area's path is placed in the area's space by its own
    /// [`Graphic::to_page`] first.
    pub fn clip_to_page(&self, area: &ClipArea) -> Matrix {
        let m = area.ctm.unwrap_or(Matrix::IDENTITY);
        let m = if self.clips_follow_ctm { m.then(&self.ctm.unwrap_or(Matrix::IDENTITY)) } else { m };
        m.then(&Matrix::translate(self.boundary.x, self.boundary.y))
    }
}

/// `Clips/Clip`: one clipping region, the union of its areas.
#[derive(Clone, Debug, PartialEq)]
pub struct Clip {
    pub areas: Vec<ClipArea>,
}

/// `Clip/Area`: a path or a text that is part of a clipping region.
#[derive(Clone, Debug, PartialEq)]
pub struct ClipArea {
    pub ctm: Option<Matrix>,
    pub draw_param: Option<u32>,
    pub shape: ClipShape,
}

/// What a clip area is made of.
#[derive(Clone, Debug, PartialEq)]
pub enum ClipShape {
    /// `Area/Path`: the inside of the path, by its fill rule. Its own `Clips` are not read.
    Path(Box<PathObject>),
    /// `Area/Text`: the outlines of a text. Not read yet.
    Text,
    /// An area with neither a path nor a text.
    Empty,
}

/// A colour (`CT_Color`). `value` is in the colour space's components (0–255 for 8-bit RGB).
#[derive(Clone, Debug, PartialEq)]
pub struct Color {
    pub value: Vec<f64>,
    pub color_space: Option<u32>,
    pub index: Option<u32>,
    pub alpha: u8,
    /// The colour is a pattern or shading (axial, radial, Gouraud) rather than a plain value.
    pub shading: bool,
    /// The axial or radial shading, when `shading` is one and could be read. Gouraud shadings
    /// are not read yet.
    pub shade: Option<Box<Shade>>,
    /// The tiling pattern, when `shading` is one and could be read.
    pub pattern: Option<Box<Pattern>>,
}

/// `Pattern`: a cell of drawing repeated every `x_step` × `y_step`. Its space is the object's
/// boundary space (the object's CTM does not apply), through the pattern's own `ctm`.
#[derive(Clone, Debug, PartialEq)]
pub struct Pattern {
    pub width: f64,
    pub height: f64,
    /// `XStep`/`YStep`; the cell's size when not given.
    pub x_step: f64,
    pub y_step: f64,
    /// `ReflectMethod` other than `Normal`: cells are mirrored in rows or columns.
    pub reflect: bool,
    /// `RelativeTo="Page"`: the cells line up with the page, not the object.
    pub relative_to_page: bool,
    pub ctm: Option<Matrix>,
    /// `CellContent`.
    pub objects: Vec<Object>,
}

/// `AxialShd` or `RadialShd`: colours blended between two points. The points are in the
/// object's boundary space (its corner at the origin; the object's CTM does not apply).
#[derive(Clone, Debug, PartialEq)]
pub struct Shade {
    pub kind: ShadeKind,
    pub start: (f64, f64),
    pub end: (f64, f64),
    /// `Extend`: colour beyond the start point, beyond the end point.
    pub extend_start: bool,
    pub extend_end: bool,
    /// `MapType` other than `Direct`: the blend repeats or reflects.
    pub repeat: bool,
    /// `Segment`s in order.
    pub stops: Vec<ShadeStop>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ShadeKind {
    Axial,
    /// Circles around the start and end points; an `eccentricity` other than 0 makes them
    /// ellipses.
    Radial {
        start_radius: f64,
        end_radius: f64,
        eccentricity: f64,
    },
}

/// One `Segment`: a colour at a position from 0 (start) to 1 (end), if given.
#[derive(Clone, Debug, PartialEq)]
pub struct ShadeStop {
    pub position: Option<f64>,
    pub value: Vec<f64>,
    pub color_space: Option<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FillRule {
    NonZero,
    EvenOdd,
}

/// One command of `AbbreviatedData`. `S` (start) and `M` (move) are both [`PathCmd::Move`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PathCmd {
    Move(f64, f64),
    Line(f64, f64),
    /// Quadratic Bézier: control point, end point.
    Quad(f64, f64, f64, f64),
    /// Cubic Bézier: two control points, end point.
    Cubic(f64, f64, f64, f64, f64, f64),
    /// Elliptical arc (as in SVG): radii, x-axis rotation in degrees, flags, end point.
    Arc {
        rx: f64,
        ry: f64,
        angle: f64,
        large: bool,
        sweep: bool,
        x: f64,
        y: f64,
    },
    Close,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PathObject {
    pub graphic: Graphic,
    pub stroke: bool,
    pub fill: bool,
    pub rule: FillRule,
    pub fill_color: Option<Color>,
    pub stroke_color: Option<Color>,
    pub path: Vec<PathCmd>,
}

/// `TextCode`: a run of characters starting at (`X`, `Y`) in the object's space.
#[derive(Clone, Debug, PartialEq)]
pub struct TextCode {
    pub x: Option<f64>,
    pub y: Option<f64>,
    /// `DeltaX` with `g` repeats expanded: the step from each character to the next.
    pub delta_x: Vec<f64>,
    pub delta_y: Vec<f64>,
    pub text: String,
}

impl TextCode {
    /// Each character with its offset from (`X`, `Y`). An x offset is `None` from the first
    /// character `DeltaX` doesn't reach: it then follows the font's advances, which only a
    /// renderer knows. Without `DeltaY`, every character is on the starting line.
    pub fn glyph_offsets(&self) -> Vec<(char, Option<f64>, Option<f64>)> {
        let (mut dx, mut dy) = (Some(0.0), Some(0.0));
        let mut out = Vec::new();
        for (i, c) in self.text.chars().enumerate() {
            if i > 0 {
                dx = dx.and_then(|x| self.delta_x.get(i - 1).map(|d| x + d));
                if !self.delta_y.is_empty() {
                    dy = dy.and_then(|y| self.delta_y.get(i - 1).map(|d| y + d));
                }
            }
            out.push((c, dx, dy));
        }
        out
    }
}

/// `CGTransform`: characters `code_position..+code_count` of the text are drawn with `glyphs`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CgTransform {
    pub code_position: u32,
    pub code_count: u32,
    pub glyph_count: u32,
    pub glyphs: Vec<u32>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TextObject {
    pub graphic: Graphic,
    pub font: u32,
    /// Font size in millimetres.
    pub size: f64,
    pub stroke: bool,
    pub fill: bool,
    pub h_scale: f64,
    pub read_direction: u16,
    pub char_direction: u16,
    pub weight: u16,
    pub italic: bool,
    pub fill_color: Option<Color>,
    pub stroke_color: Option<Color>,
    pub cg_transforms: Vec<CgTransform>,
    pub codes: Vec<TextCode>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ImageObject {
    pub graphic: Graphic,
    pub resource_id: u32,
    pub substitution: Option<u32>,
    pub image_mask: Option<u32>,
}

/// Places a composite graphic unit: the unit's own space is mapped to the page like any
/// object's ([`Graphic::to_page`]).
#[derive(Clone, Debug, PartialEq)]
pub struct CompositeObject {
    pub graphic: Graphic,
    pub resource_id: u32,
}

/// `CompositeGraphicUnit`: a drawing defined once as a resource and placed on pages by
/// [`CompositeObject`]s. Its objects are in its own space, `width` × `height`.
#[derive(Clone, Debug, PartialEq)]
pub struct CompositeUnit {
    pub width: f64,
    pub height: f64,
    pub objects: Vec<Object>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PageBlock {
    pub id: Option<u32>,
    pub objects: Vec<Object>,
}

/// Resources by ID.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Resources {
    pub color_spaces: BTreeMap<u32, ColorSpace>,
    pub draw_params: BTreeMap<u32, DrawParam>,
    pub fonts: BTreeMap<u32, Font>,
    pub media: BTreeMap<u32, MultiMedia>,
    pub composites: BTreeMap<u32, CompositeUnit>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ColorSpaceKind {
    Gray,
    Rgb,
    Cmyk,
    Other(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct ColorSpace {
    pub kind: ColorSpaceKind,
    pub bits_per_component: u8,
    /// Entry name of an ICC profile, if one is given.
    pub profile: Option<String>,
}

/// `DrawParam`: shared drawing parameters, possibly based on another one (`relative`).
#[derive(Clone, Debug, PartialEq)]
pub struct DrawParam {
    pub relative: Option<u32>,
    pub line_width: Option<f64>,
    pub join: Option<String>,
    pub cap: Option<String>,
    pub miter_limit: Option<f64>,
    pub fill_color: Option<Color>,
    pub stroke_color: Option<Color>,
}

/// `Font`. Without `file` the font is referenced by name only (common for 宋体/SimSun and
/// 楷体/KaiTi in invoices) and must be substituted.
#[derive(Clone, Debug, PartialEq)]
pub struct Font {
    pub name: String,
    pub family: Option<String>,
    pub charset: Option<String>,
    pub italic: bool,
    pub bold: bool,
    pub serif: bool,
    pub fixed_width: bool,
    /// Entry name of the embedded font file.
    pub file: Option<String>,
}

/// `MultiMedia` (images, mostly).
#[derive(Clone, Debug, PartialEq)]
pub struct MultiMedia {
    /// `Type`: "Image", "Audio" or "Video".
    pub kind: String,
    /// `Format`, e.g. "PNG" or "JPEG", as written; the file's bytes are the authority.
    pub format: Option<String>,
    /// Entry name of the media file.
    pub file: String,
}
