//! Prepare HTML form data for an explicitly requested SubmitForm POST. Network transport and
//! user consent live at L7; this layer only reads the current, unsaved form values.

use pdfcraft_forms::{Field, FieldKind, flags};

use crate::{DataFormat, DocId, EditError, Session, guard};

/// SubmitForm's field selection. `names: None` means all fields; an empty list means none when
/// including, or all when excluding. A named non-terminal field includes its descendants.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SubmitOptions {
    pub names: Option<Vec<String>>,
    pub exclude: bool,
    pub include_empty: bool,
}

impl SubmitOptions {
    fn selects(&self, field: &Field) -> bool {
        if matches!(field.kind, FieldKind::PushButton | FieldKind::Signature) || field.has(flags::NO_EXPORT) {
            return false;
        }
        let Some(names) = &self.names else { return true };
        let named = names.iter().any(|name| field.name == *name || field.name.strip_prefix(name).is_some_and(|tail| tail.starts_with('.')));
        if self.exclude { !named } else { named }
    }
}

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
        self.html_form_selected(id, &SubmitOptions::default())
    }

    pub fn html_form_selected(&self, id: DocId, options: &SubmitOptions) -> Result<HtmlForm, EditError> {
        let doc = self.get(id).ok_or(EditError::NoDocument)?;
        let mut pairs = Vec::new();
        for field in doc.form.iter() {
            if !options.selects(field) {
                continue;
            }
            if field.has(flags::REQUIRED) && field.value.iter().all(String::is_empty) {
                return Err(EditError::Invalid(format!("required field {:?} has no value", field.name)));
            }
            if field.has(flags::FILE_SELECT) && field.value.iter().any(|v| !v.is_empty()) {
                return Err(EditError::Invalid(format!("file-select field {:?} cannot be submitted as HTML form data", field.name)));
            }
            if options.include_empty && field.value.is_empty() {
                pairs.push((field.name.as_str(), ""));
            }
            for value in field.value.iter().filter(|v| options.include_empty || !v.is_empty()) {
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

    /// Serialize the current, unsaved form as a filtered FDF or XFDF submission. Exported file
    /// data remains separate from the source PDF; nothing is saved to disk.
    pub fn form_data_submission(
        &self,
        id: DocId,
        format: DataFormat,
        options: &SubmitOptions,
        include_annotations: bool,
    ) -> Result<(Vec<u8>, usize), EditError> {
        if !matches!(format, DataFormat::Fdf | DataFormat::Xfdf) {
            return Err(EditError::Invalid("form data submission requires FDF or XFDF".into()));
        }
        let doc = self.get(id).ok_or(EditError::NoDocument)?;
        let mut names = Vec::new();
        for field in doc.form.iter() {
            if !options.selects(field) {
                continue;
            }
            if field.has(flags::REQUIRED) && field.value.iter().all(String::is_empty) {
                return Err(EditError::Invalid(format!("required field {:?} has no value", field.name)));
            }
            if field.has(flags::FILE_SELECT) && field.value.iter().any(|v| !v.is_empty()) {
                return Err(EditError::Invalid(format!("file-select field {:?} cannot be submitted as form data", field.name)));
            }
            if options.include_empty || field.value.iter().any(|v| !v.is_empty()) {
                names.push(field.name.clone());
            }
            if names.len() > 10_000 {
                return Err(EditError::Invalid("the form has too many fields to submit".into()));
            }
        }
        let cos = match doc.editor.as_ref() {
            Some(editor) => editor.cos.clone(),
            None => {
                pdfcraft_cos::Document::open_with_password(doc.bytes.clone(), doc.password.as_deref()).map_err(|e| EditError::Write(e.to_string()))?
            }
        };
        let bytes = guard(|| {
            if format == DataFormat::Fdf {
                pdfcraft_xfdf::export_fdf_submission(&cos, &doc.name, &names, include_annotations)
            } else {
                pdfcraft_xfdf::export_xfdf_submission(&cos, &doc.name, &names).into_bytes()
            }
        })
        .map_err(EditError::Write)?;
        Ok((bytes, names.len()))
    }
}
