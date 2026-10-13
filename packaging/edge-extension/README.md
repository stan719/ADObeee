# Microsoft Edge extension

The release extension is a local wrapper around the PdfCraft web build. The package is created by
`packaging/web/package.sh`, which first builds `dist/web` and then runs:

```sh
cargo xtask edge-extension --site dist/web --out target/edge-extension
```

The xtask copies the static web build, removes hosting-only files that Edge reserves or does not
need, extracts Trunk's generated inline module startup and styles into external files, and writes
the Manifest V3 manifest plus a static launcher page. The bootstrap is generated from Trunk output;
the repository does not add handwritten JavaScript.

## Install from a release

1. Download `pdfcraft-edge-extension-<version>.zip` from the GitHub release and extract it.
2. In Edge, open `edge://extensions` and enable **Developer mode**.
3. Choose **Load unpacked** and select the extracted `pdfcraft-edge-extension-<version>` folder,
   the one containing `manifest.json`.
4. Pin PdfCraft from Edge's Extensions menu. Click its toolbar button, then click **Open PdfCraft**
   in the launcher to open the app in a tab.
5. Use PdfCraft's Open control to choose a PDF.

Keep the extracted folder in place while the extension is loaded. Edge will reload it from that
folder. To update, extract the new release to a stable folder and use **Reload** on the extension
card after selecting the new folder with **Load unpacked**.

The extension does not intercept PDFs or replace Edge's built-in PDF viewer. It requests no
extension permissions. The web app processes selected files in the browser; this wrapper adds no
network requests.
