# Review and remove scan lines

Open **Scan & OCR → Enhance scanned file**, choose a bitmap on the current page and
select **Detect scan lines**. The preview outlines possible lines and numbers them.
Check only the defects you want removed, then select **Apply**. Nothing is selected
automatically. Undo restores the original image in one step.

The detector proposes isolated, long, thin dark components with bright paper beside
them. It supports horizontal and vertical edge lines and narrow, nearly straight
crease marks. Short text strokes, connected table grids, and lines connected to text
are conservatively excluded. An isolated printed rule can still look like a scan
defect: always review the preview. This does not reconstruct text hidden by a crease,
remove broad fold shadows, deskew a page, or run OCR.

Only the selected component pixels are replaced, interpolating the adjacent paper
colour. Other bitmap pixels, image placement, page text/OCR, annotations and other
page contents are retained. The replacement is lossless PNG, so file size can change.
The initial implementation accepts opaque bitmap images up to 16 million pixels;
grouped Form artwork and transparent images are refused. One bitmap is reviewed at
a time. Changing the document, page or image invalidates the preview. On desktop,
detection and encoding use a single background worker; the bounded web operation
runs synchronously.

## CLI and MCP

Both tools use the current document snapshot, including unsaved edits. Page, image
and candidate numbers are **1-based**. Candidate rectangles are bitmap pixel
coordinates with a top-left origin and exclusive right/bottom edges.

1. Call `scan_cleanup_preview` with `doc`, `page` and `image`. It returns `generation`,
   image dimensions and numbered candidate rectangles. This is read-only.
2. Review the image and candidate rectangles.
3. Call `scan_cleanup_apply` with the same document/page/image, the returned
   `generation` and a nonempty `candidates` array of reviewed IDs.

An intervening edit, undo or redo invalidates the generation. Invalid or duplicate
IDs produce an error, not a partial edit. Copy and modification permissions are
required. The tools have no filesystem or network side effects; normal save and
export tools retain their root confinement.

The engine exposes `Document::scan_cleanup_source`, `Source::detect`, and
`Preview::edit`. Before applying a prepared edit, call
`Document::check_scan_cleanup` to reject a stale or foreign proposal. Applications
can use `Preview::cleaned_rgba` for a full-resolution before/after review.
