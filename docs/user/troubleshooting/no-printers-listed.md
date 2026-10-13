---
title: No printers are listed in the Print dialog
description: Why the PdfCraft Print dialog can show no printers on Windows 11 and Linux, and how to fix it or print meanwhile.
order: 20
verified: 0.7.1
---

Since 0.7.0, PdfCraft lists your Windows printers. If you still see only **Save as PDF**, update to the latest release. On Linux, check that CUPS is running. To print right away, choose **Save as PDF** and print the saved file from another app.

Shortcuts on this page use Ctrl. On macOS, use Cmd instead of Ctrl.

## Symptoms

- **File**, **Print…** (`Ctrl+P`) opens the **Print** dialog, but the **Printer:** list has no printers.
- Only **Save as PDF** is available ([issue #1031](https://github.com/storytold/pdfcraft/issues/1031), [issue #1084](https://github.com/storytold/pdfcraft/issues/1084)).

## Fix

### Windows 11

1. Check your version in **Help**, **About PdfCraft**. Version 0.6.0 and earlier cannot list printers on Windows. Install the latest release: see [Install PdfCraft](../getting-started/install.md).
2. If the list is still empty, choose **Save as PDF** in the **Print** dialog and save the file.
3. Open the saved PDF in another app and print it from there. A reporter printed with Google Chrome.

PdfCraft asks Windows PowerShell for the printer list. On a PC where PowerShell is locked down, for example by App Control for Business script enforcement, the list stays empty. This is open as [issue #1107](https://github.com/storytold/pdfcraft/issues/1107).

### Linux

Printers come from CUPS. A user in issue #1084 reported that printing works on Arch Linux when CUPS is set up and working. Check that your printer shows in your system's printer settings first. PdfCraft also lists CUPS printers that have no permanent queue.

## Why this happens

PdfCraft 0.6.0 and earlier had no way to list printers on Windows. Printing for Windows ([pull request #847](https://github.com/storytold/pdfcraft/pull/847)) came in 0.7.0. When PdfCraft cannot get a printer list from the system, the dialog offers only **Save as PDF**.

## Still stuck?

Ask in [Discord](https://discord.gg/artcraft) or comment on [issue #1084](https://github.com/storytold/pdfcraft/issues/1084). Say your system, your PdfCraft version from **Help**, **About PdfCraft**, and the printer model.
