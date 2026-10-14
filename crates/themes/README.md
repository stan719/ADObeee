# pdfcraft-themes

L7 GUI-independent application theme domain shared by the egui shell and automation.
`ThemeState::execute` is the validated operation boundary; `ThemeStore` adds native
locking, revision checks, legacy migration and atomic persistence. Neither depends
on an egui context or a document session. Native adapters should use ThemeStore;
browser hosts use ThemeState with their persistence backend.

See [theme architecture, schema and commands](../../docs/themes.md).
