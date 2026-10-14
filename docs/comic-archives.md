# Comic archives and batch conversion

Open a **CBZ** or **ZIP** image archive, or a **CBT** or uncompressed **TAR**
image archive, using File > Open or drag and drop. Its images become pages in a
new, unsaved PDF. Save asks for a PDF destination; the original archive is not
overwritten. The same formats work in Create > Multiple files and Combine.

Pages follow **archive entry order**, including images in subfolders. Filenames
are not sorted. Image dimensions, resolution and the existing image importer’s
colour/transparency handling are retained. Supported page images are PNG, JPEG,
JPEG 2000, TIFF (all pages), GIF (first frame) and BMP. Hidden files, macOS resource
forks, directories and non-image metadata such as ComicInfo.xml are ignored.

A damaged page fails that archive's conversion, rather than silently returning
an incomplete book. Unsupported WebP/AVIF/HEIC/JXL/SVG pages, encrypted ZIP pages
and image entries that are links are refused. CBR/RAR, CB7/7z, compressed TAR,
EPUB and XPS are not supported by this importer.

Archives are read in memory without extracting paths. Limits are 512 MiB for
the input archive and the total decompressed image files, 64 MiB per image file,
10,000 entries and 4,096 PDF pages. Image decoders also apply their own limits.
Large books can take time to convert; this does not add a background job or cancel
button to the existing Create flow.

## CLI and MCP

The existing `doc_create_multiple` tool accepts one archive or a batch. Save this
as `convert.json`:

```json
[
  {"tool":"doc_create_multiple","args":{"paths":["comic.cbz"],"out":"comic.pdf"}}
]
```

Run it with paths confined to your working folder:

```sh
pdfcraft-cli run --root . --script convert.json
```

For one PDF per input, use:

```json
[
  {"tool":"doc_create_multiple","args":{"paths":["first.cbz","second.cbt","photos.zip"],"mode":"separate","out_dir":"pdfs"}}
]
```

Separate mode reports each file's output or error and continues after an invalid
archive. Existing outputs receive numbered names and are not replaced. The
default `combine` mode joins all selected archives into one PDF in input order;
if an input fails, no partial combined PDF is written. `pages` can select ranges
from each converted source, using the existing tool schema. MCP uses the same
tool and arguments.
