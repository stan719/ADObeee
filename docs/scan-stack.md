# Scan stack

Scanning is Windows/WIA only in this version. On other platforms the same code builds and every
scan request reports one sentence ("scanning is only supported on Windows (WIA) in this version");
other backends are future work behind the same seam.

| Crate | Role |
|------|------|
| `pdfcraft-scan` | Platform-independent rules: scan settings, errors and recovery, document-handling status, the acquire contract (progress and cancel tokens, in-memory pages), software 1-bit conversion, a BMP parser, tracing, and the `unsupported` backend. No platform code and no unsafe. |
| `pdfcraft-wia` | The Windows WIA backend (COM). The only crate in the stack with `unsafe`; see [unsafe-audit.md](unsafe-audit.md). |
| `pdfcraft-platform` | `pdfcraft_platform::scan`: re-exports the contract and picks the backend (`pdfcraft-wia` under `cfg(windows)`, `unsupported` elsewhere). `scan::supported()` answers at run time. |
| `pdfcraft-create` | Keeps a black-and-white 1-bit BMP as a 1-bit image when creating a PDF. |
| `pdfcraft-automation` | The `scan_devices` and `scan_acquire` tools (and their MCP form). |
| `pdfcraft-cli` | `pdfcraft-cli scan devices` and `pdfcraft-cli scan --out DIR --yes`; see [cli.md](cli.md). |

Devices, feeder, WSD and duplex behaviour are described in
[hardware-parity.md](hardware-parity.md#windows-wia-scanner-acquire).

Tests never open real scanners unless `PDFCRAFT_TEST_REAL_SCANNERS` is set. `PDFCRAFT_SCAN_DENY`
names devices that are never opened, and `PDFCRAFT_SCAN_TRACE` records the acquire phases.
