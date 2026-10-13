# pdfcraft-optimize

Layer L4: the PDF Optimizer (execution plan M11.1; Acrobat's Reduce File Size and Optimize PDF ▸
Advanced optimization).

```rust
let report = optimize(&mut doc, &Settings::default())?;   // Reduce File Size's choices
let report = optimize(&mut doc, &Settings::lossless())?;  // preserve image data and user data
// Or with progress: called before each image and the clean-up; `false` cancels.
let report = optimize_with_progress(&mut doc, &settings, &mut |stage| keep_going)?;
```

- **Images:** the effective resolution of each image is measured where pages draw it (the CTM
  through form XObjects; the smallest over all uses). Colour and grayscale images above a
  threshold are resampled with a bicubic (Catmull-Rom) filter to a target resolution and
  stored as JPEG (Acrobat's quality steps) or ZIP; soft masks are resampled with their image.
  An image is replaced only if the result is smaller. Left alone: CMYK and other colour spaces,
  image masks, non-default decode arrays, more than 8 bits, JPEG 2000, JBIG2 and CCITT.
  Dimensions and decoded sample lengths are checked before allocation, for images and soft
  masks alike; unsupported/malformed images stay unchanged. A resampled soft mask gets a new
  reference belonging to the changed image, so other users of the original mask keep it.
- **Discard objects:** thumbnails, alternate images, document tags, print settings.
- **Clean up:** Flate for streams with no filter; for streams with an outer ASCII85 or ASCIIHex
  layer, that layer is removed when the result is smaller (the inner filters keep their encoded
  bytes). Streams with `/DecodeParms`, images and object streams are left alone.

The engine runs Remove Hidden Information (`pdfcraft-redact`) for the user-data categories,
then this, then merges identical objects and writes a full, compressed save
(`pdfcraft_engine::optimizer::OptimizeJob`, which the UI runs on a worker thread with a
progress bar). JPEG decoding and
encoding and resampling come from the `image` crate.

`Settings::lossless()` disables downsampling, image re-encoding, all discard flags, link and
named-destination removal, and unused-XObject pruning. Encoded and raw image samples stay exact;
non-image streams and the full-save structure may be compressed. The engine can still merge
identical resources. Pass an empty `discard` list to `Session::optimized_bytes`; already absent
categories are a successful no-op when a caller explicitly selects them. The existing
`doc_optimize` tool exposes the profile as `profile: "lossless"` and refuses contradictory custom
options. `Settings::default()` and `doc_reduce` keep their existing lossy choices.

This is not a byte-identical archival copy: a full rewrite drops old revisions and may update the
modification date. It is not guaranteed to be smaller. Signed PDFs are refused by the engine.
UI preset management, monochrome/JBIG2/CCITT re-encoding and broader real-scan quality benchmarks
remain separate work. The full-save reference-generation correction is tracked in upstream
[PR #1143](https://github.com/storytold/pdfcraft/pull/1143); this change does not duplicate it.
