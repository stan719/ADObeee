# Text extraction limits

Text extraction has a conservative **64 MiB per-page allocation allowance** for collected
text and glyph bookkeeping. It charges each ActualText invocation before copying and
decoding its replacement, including repeated references to the same named property.
Glyph storage, retained originals and additional replacement copies for unmarked redraws
also consume the allowance. Charges are monotonic during a page, even when an invalid
glyph or an unmergeable replacement is subsequently discarded.

The allowance estimates allocation work, not the PDF file size or total process memory.
PDF/font parsing, raster buffers and the completed-page cache have separate lifetimes.
A page can therefore reach the allowance with less than 64 MiB of final extracted text.

When the allowance is exhausted, extraction returns an explicit page-specific error
and no partial PageText. The renderer still draws the page, and later pages get a fresh
allowance. Existing callers receive the normal extraction-error result; this is not a
successful empty or truncated page. Normal ligatures and PDFDocEncoding, UTF-16 and UTF-8
ActualText continue through the existing decoding and layout paths.

Regression coverage lives in crates/render/src/text.rs: small-budget repeated-property,
empty replacement, Unicode and redraw tests, plus a PageRenderer test that exercises the
production limit and verifies continued pixel rendering and text extraction on another page.
