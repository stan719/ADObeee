//! Prepare HTML form data for an explicitly requested SubmitForm POST. Network transport and
//! user consent live at L7; this layer only reads the current, unsaved form values.

use pdfcraft_forms::{FieldKind, flags};

use crate::{DocId, EditError, Session};

/// A bounded `application/x-www-form-urlencoded` body from the current form state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HtmlForm {
    pub body: Vec<u8>,
    pub fields: usize,
}

impl Session {
    /// Prepare an HTML SubmitForm body. `/Ff` NoExport fields, signatures, push buttons and
    /// fields without a value are excluded. A multi-select choice sends one pair per value.
    pub fn html_form(&self, id: DocId) -> Result<HtmlForm, EditError> {
        let doc = self.get(id).ok_or(EditError::NoDocument)?;
        let mut pairs = Vec::new();
        for field in doc.form.iter() {
            if matches!(field.kind, FieldKind::PushButton | FieldKind::Signature) || field.has(flags::NO_EXPORT) {
                continue;
            }
            if field.has(flags::REQUIRED) && field.value.iter().all(String::is_empty) {
                return Err(EditError::Invalid(format!("required field {:?} has no value", field.name)));
            }
            if field.has(flags::FILE_SELECT) && field.value.iter().any(|v| !v.is_empty()) {
                return Err(EditError::Invalid(format!("file-select field {:?} cannot be submitted as HTML form data", field.name)));
            }
            for value in field.value.iter().filter(|v| !v.is_empty()) {
                let export = match field.kind {
                    FieldKind::CheckBox | FieldKind::Radio => field.export_for_state(value),
                    _ => value,
                };
                pairs.push((field.name.as_str(), export));
                if pairs.len() > 10_000 {
                    return Err(EditError::Invalid("the form has too many values to submit".into()));
                }
            }
        }
        let fields = pairs.len();
        if fields == 0 {
            return Err(EditError::Invalid("the form has no values to submit".into()));
        }
        let body = url::form_urlencoded::Serializer::new(String::new()).extend_pairs(pairs).finish().into_bytes();
        if body.len() > 8 * 1024 * 1024 {
            return Err(EditError::Invalid("the form data is larger than 8 MiB".into()));
        }
        Ok(HtmlForm { body, fields })
    }
}
