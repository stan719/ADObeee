use super::*;
use pdfcraft_cos::Object;
use std::io::Write;

fn png(width: u32) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, width, 2);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.write_header().unwrap().write_image_data(&vec![120; width as usize * 6]).unwrap();
    }
    out
}

fn archive(format: ArchiveFormat, entries: &[(&str, Vec<u8>)]) -> Vec<u8> {
    match format {
        ArchiveFormat::Zip => {
            let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
            for (name, bytes) in entries {
                zip.start_file(*name, zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated)).unwrap();
                zip.write_all(bytes).unwrap();
            }
            zip.finish().unwrap().into_inner()
        }
        ArchiveFormat::Tar => {
            let mut tar = tar::Builder::new(Vec::new());
            for (name, bytes) in entries {
                let mut header = tar::Header::new_gnu();
                header.set_size(bytes.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                tar.append_data(&mut header, *name, bytes.as_slice()).unwrap();
            }
            tar.into_inner().unwrap()
        }
    }
}

#[test]
fn comic_archives_preserve_entry_order_dimensions_and_saved_pages() {
    for format in [ArchiveFormat::Zip, ArchiveFormat::Tar] {
        let bytes = archive(
            format,
            &[("10.PNG", png(10)), ("ComicInfo.xml", b"metadata".to_vec()), ("chapter/2.png", png(2)), ("__MACOSX/._1.png", vec![1])],
        );
        let doc = from_comic_archive("comic.cbz", &bytes, format).unwrap();
        let pdf = pdfcraft_cos::write_full(&doc, &Default::default()).unwrap();
        let reopened = Document::open(std::sync::Arc::new(pdf)).unwrap();
        let root = reopened.get(reopened.root().unwrap()).as_dict().unwrap().reference(b"Pages").unwrap();
        let pages = reopened.get(root);
        let kids = pages.as_dict().unwrap().get(b"Kids").unwrap().as_array().unwrap();
        let widths: Vec<_> =
            kids.iter().map(|p| reopened.resolve(p).as_dict().unwrap().get(b"MediaBox").unwrap().as_array().unwrap()[2].as_f64().unwrap()).collect();
        assert_eq!(widths, [10.0, 2.0], "entry order, not alphabetical or numeric order");
        let info = reopened.trailer().reference(b"Info").unwrap();
        assert_eq!(reopened.get(info).as_dict().unwrap().get(b"Title"), Some(&Object::String(pdfcraft_cos::PdfString::text("comic"))));
    }
}

#[test]
fn comic_archives_refuse_bad_pages_instead_of_returning_a_partial_document() {
    for format in [ArchiveFormat::Zip, ArchiveFormat::Tar] {
        for bad in ["broken.png", "unsupported.webp"] {
            let bytes = archive(format, &[("good.png", png(4)), (bad, vec![1, 2, 3])]);
            let error = from_comic_archive("comic", &bytes, format).unwrap_err().to_string();
            assert!(error.contains(bad), "{error}");
        }
        let empty = archive(format, &[("ComicInfo.xml", Vec::new()), (".hidden.png", png(2))]);
        assert!(from_comic_archive("empty", &empty, format).unwrap_err().to_string().contains("no supported"));
        assert!(from_comic_archive("broken", b"broken archive", format).is_err());
    }
}

#[test]
fn archive_limits_check_declared_and_actual_sizes_and_page_count() {
    let mut pages = Pages { doc: Document::new_empty(), profiles: Vec::new(), remaining: 10, count: 0 };
    assert!(pages.read("large.png", 11, Cursor::new([])).unwrap_err().to_string().contains("limit"));
    assert!(pages.read("lying.png", 2, Cursor::new([0; 20])).unwrap_err().to_string().contains("decompressed size"));
    assert!(pages.read("short.png", 10, Cursor::new([0; 2])).unwrap_err().to_string().contains("decompressed size"));
    assert_eq!(pages.remaining, 10);
    pages.count = MAX_PAGES;
    assert!(pages.read("extra.png", 0, Cursor::new([])).unwrap_err().to_string().contains("limit"));
}

#[test]
fn tar_image_links_are_refused_and_never_extracted() {
    let mut tar = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Symlink);
    header.set_size(0);
    header.set_mode(0o644);
    tar.append_link(&mut header, "page.png", "outside.png").unwrap();
    let bytes = tar.into_inner().unwrap();
    assert!(from_comic_archive("linked.cbt", &bytes, ArchiveFormat::Tar).unwrap_err().to_string().contains("linked"));
}

#[test]
fn zip_crc_errors_are_reported() {
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    zip.start_file("page.png", zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored)).unwrap();
    zip.write_all(&png(4)).unwrap();
    let mut bytes = zip.finish().unwrap().into_inner();
    let index = bytes.windows(8).position(|w| w == b"\x89PNG\r\n\x1a\n").unwrap();
    bytes[index] ^= 1;
    assert!(from_comic_archive("bad.cbz", &bytes, ArchiveFormat::Zip).unwrap_err().to_string().contains("checksum"));
}

#[test]
fn archive_detection_is_explicit_and_does_not_capture_other_zip_formats() {
    for name in ["COMIC.CBZ", "comic.zip"] {
        assert_eq!(crate::source_kind(name, b"PK\x03\x04"), Some(crate::SourceKind::Archive(ArchiveFormat::Zip)));
    }
    for name in ["comic.cbt", "comic.TAR"] {
        assert_eq!(crate::source_kind(name, b""), Some(crate::SourceKind::Archive(ArchiveFormat::Tar)));
    }
    for name in ["book.epub", "document.docx", "archive.bin"] {
        assert_eq!(crate::source_kind(name, b"PK\x03\x04"), None);
    }
}

#[test]
fn archive_paths_accept_dot_prefixes_but_ignore_resource_forks() {
    assert!(image_entry("./chapter/01.PNG").unwrap());
    assert!(image_entry("chapter\\01.png").unwrap());
    assert!(!image_entry("chapter/.hidden/01.png").unwrap());
    assert!(!image_entry("./__MACOSX/._01.png").unwrap());
}
