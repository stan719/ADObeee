//! OFD → PDF, early alpha (#1036): an OFD file becomes a new, unsaved PDF.

use std::sync::Arc;

pub use pdfcraft_create::OfdReport;

use crate::{EditError, Session, guard};

impl Session {
    /// Convert an OFD (GB/T 33190) file to PDF bytes, with what could not be converted. The
    /// reader and converter run behind the panic guard, like every other import.
    pub fn convert_ofd(&self, name: &str, bytes: &[u8]) -> Result<(Arc<Vec<u8>>, OfdReport), EditError> {
        let converted =
            guard(|| pdfcraft_create::from_ofd(bytes)).map_err(|_| EditError::Source(format!("{name}: the OFD file could not be read")))?;
        let (doc, report) = converted?;
        Ok((self.write_new(&doc)?, report))
    }
}
