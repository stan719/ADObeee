# pdfcraft-ofd

Layer L3: reading OFD (Open Fixed-layout Document, GB/T 33190-2016), China's national
fixed-layout format for electronic invoices, government documents and archives (#1036). No
workspace dependencies.

```rust
if pdfcraft_ofd::is_ofd(&bytes) {                 // a ZIP with OFD.xml at its root (.docx is not)
    let doc = pdfcraft_ofd::parse(&bytes)?;       // or parse_with_limits(&bytes, &limits)
    for page in &doc.pages {
        let (w, h) = page.area.physical.size_pt(); // PDF points
        let to_pdf = page.area.physical.page_to_pdf();
        for layer in &page.content.layers { /* Object::Path / Text / Image / Composite / Block */ }
    }
}
```

## What it reads

- **Container** (§6), `zip::Package`: stored and Deflate entries from the central directory.
  Refused with an error that says why: ZIP64, multi-disk and encrypted archives, other
  compression methods, names with `..`, absolute or drive paths, duplicate names, a wrong CRC,
  an entry whose unpacked size differs from its header, and anything over `Limits` (entry count,
  per-entry and total unpacked size, compression ratio over 200× for entries above 1 MiB).
- **Structure** (§7): `OFD.xml` (first `DocBody`; `DocInfo`, `DocRoot`, `Signatures` location),
  `Document.xml` (`CommonData/PageArea`, `PublicRes`, `DocumentRes`, `TemplatePage`, `Pages`),
  resource files (`ColorSpaces`, `DrawParams`, `Fonts` with `FontFile`, `MultiMedias` with
  `MediaFile`, all resolved against the file's `BaseLoc`), and each page's `Content.xml`
  (`Template`, `PageRes`, `Area`, `Content/Layer`), and the annotations (`Annotations` ▸ each
  page's `PageAnnot` ▸ `Annot`: type, visibility, `Appearance` boundary and objects) into
  `Page::annotations`.
- **Objects**: `PathObject` (`AbbreviatedData`: `S`/`M`, `L`, `Q`, `B`, `A`, `C`), `TextObject`
  (`TextCode` with `X`/`Y` and `DeltaX`/`DeltaY` where `g N v` repeats `v` N times;
  `CGTransform` glyph indices), `ImageObject`, `CompositeObject` (its unit, a resource, in
  `Resources::composites`), nested
  `PageBlock`, with the common `Boundary`, `CTM`, `DrawParam`, `LineWidth`, `Alpha`, `Visible`,
  fill/stroke colours (plain values, `AxialShd`/`RadialShd` blends as `Color::shade` and
  tiling `Pattern`s as `Color::pattern`; Gouraud shadings are not read yet) and `Clips` (`Clip` regions intersect, the `Area`s of one region form a
  union; an area is a `Path`, read, or a `Text`, recorded but not read yet; `TransFlag` says
  whether the object's CTM applies). `Graphic::clip_to_page` places an area in page space.

Numbers stay in OFD's units (millimetres, y down). `geom` converts: `mm_to_pt`,
`OfdBox::size_pt`, `OfdBox::page_to_pdf` (OFD page space to PDF page space) and
`Graphic::to_page` (an object's CTM, then its boundary's corner).

Elements are matched by local name, so files with and without the `ofd:` prefix read alike.
Required things that are missing or malformed are errors naming the part; unknown elements are
ignored; what is tolerated or not read yet (Gouraud shadings, extra `DocBody` elements,
references to undefined fonts, images, templates or composite graphic units) is listed in
`OfdDocument::warnings`, each message once. Tolerated because real writers produce them (all
seen in the ofdrw test files): a listed resource file missing from the package (its resources
are then undefined), an `Area` without `PhysicalBox` (its `ApplicationBox` stands in, else the
document's size), an object without `Boundary` (its coordinates are page coordinates) and a
`Boundary` with a negative size (the corner is kept, the size made positive); an image or
composite object without `ResourceID` is left out.

## Robustness

Every input is untrusted (`AGENTS.md` §4): no `unwrap`/`expect`/`panic` outside tests, checked
offsets in the ZIP reader, numbers must be finite and within ±10⁹, arrays and `g` expansions are
capped (`Limits::max_numbers`; a `DeltaX` keeps only the steps its text needs), XML parts are
size- and node-bounded with DTDs refused, page blocks nest at most `Limits::max_depth`, and
`..` can't leave the package. Document-wide budgets bound what a small file can make the reader
hold: XML read in total (`max_xml_total`, counting a content file shared by many pages once per
page), objects (`max_objects`), the model's memory (`max_model_bytes`) and text characters
(`max_text_chars`). `parse_package` reads from a package the caller opened, so the images and
fonts it reads afterwards share one unpacking budget. `parse_with_limits` is the fuzzing entry point; the tests include
property tests that damage packages, XML and path data at random.

## Not yet (phase 2 and later)

Phase 2 plugs this model into the existing conversion path rather than adding a new one:
`pdfcraft-create` gains `SourceKind::Ofd` (detected with `is_ofd`, after the PDF and image
checks) and a converter that writes each page as a PDF page — paths, images (through the same
image embedding as Create), text with embedded fonts, and name-only fonts (宋体/SimSun,
楷体/KaiTi) as non-embedded CID fonts so the reader's Chinese fallback draws them. Then
`Session::convert_to_pdf`, and with it Open, Insert, Create ▸ Multiple files, Combine files and
`doc_create_multiple`, accept `.ofd` without further changes; `CONVERTIBLE` gains `ofd` for the
file pickers. Electronic seals (GB/T 38540) would be shown as images, with the converted PDF
saying the original OFD signature is not carried over or verified. Annotations, outlines,
attachments, custom tags, shadings and composite units come after that. Writing OFD is out of
scope.

Tests build their OFD packages in code (`src/tests.rs`); no binary fixtures and no real
invoices are committed.
