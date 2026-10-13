# PdfCraft CLI

Command-line entry points live in the `pdfcraft-cli` crate (see the repository README for the full command list).

## Scan (`pdfcraft-cli scan`)

Scanning is Windows/WIA only in this version: on other platforms `pdfcraft-cli scan` exits with the line `scan: scanning is only supported on Windows (WIA) in this version`, and the `scan_devices` and `scan_acquire` tools return the same sentence. Other backends are future work behind the same seam.

Windows acquire uses WIA through `pdfcraft-wia` (the COM layer; `pdfcraft-scan` holds the shared rules). See [hardware-parity.md](hardware-parity.md#windows-wia-scanner-acquire) for devices, feeder, WSD, and duplex behavior.

**Diagnostics**

| Variable | Effect |
|----------|--------|
| `PDFCRAFT_SCAN_TRACE` | `1` or `true`: append one JSON object per acquire phase to stderr. Any other non-empty value is treated as a path and lines are appended to that JSONL file. Phases include `device_open`, `props_written`, `transfer_start`, `transfer_end`, and `status_read` (handling bits only). No device names, ids, ports, or image data. |
| `PDFCRAFT_SCAN_STATUS_LOG` | `1`: log each WIA transfer STATUS callback (`percent`, `transferred` bytes) to stderr during file transfer. |

Example (stderr trace during acquire):

```powershell
$env:PDFCRAFT_SCAN_TRACE = "1"
pdfcraft-cli scan acquire --device "…" --source feeder --pages 1 --out-dir .\out
```

Manual check: run a short acquire with `PDFCRAFT_SCAN_TRACE=1` and confirm phase lines appear on stderr in order through `transfer_end`.
