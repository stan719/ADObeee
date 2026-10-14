# Mirror PDF pages

In **Organize pages**, select the pages to change, then choose **Flip horizontal**
(left to right) or **Flip vertical** (top to bottom). The same actions are in the
Pages menu and command palette. Without an explicit selection the interface uses
the current page. Select all pages to mirror the entire document.

Directions follow the displayed page, including its current rotation. The visible
CropBox stays in place; page dimensions and rotation are unchanged. Content remains
vector/text content, rather than becoming a raster image. Annotation and link
rectangles, drawing coordinates and stored appearances move with the content;
form values and link actions are retained. Malformed geometry or annotations shared
between pages are refused atomically.

Each operation is one undo step. Applying the same flip twice restores the original
appearance. Save writes the mirrored pages to the PDF, so the change is also visible
when it is reopened in another reader. This requires permission to modify document
content; permission only to assemble pages is insufficient.

## CLI and MCP

Both use the same tools. Pages are 1-based; omitted `pages` means **all pages**,
and duplicate page numbers are processed once:

```json
{"tool":"page_flip_horizontal","args":{"doc":1,"pages":[1,3]}}
{"tool":"page_flip_vertical","args":{"doc":1}}
```

The command IDs are `page.flip_horizontal` and `page.flip_vertical`.
