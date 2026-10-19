//! Office Open XML / OpenDocument: text extraction and a minimal DOCX writer.

use std::io::{Read, Write};
use std::path::Path;

use anyhow::{Context, Result};
use quick_xml::events::Event;
use quick_xml::Reader;

fn zip_entry(path: &Path, name: &str) -> Result<String> {
    let f = std::fs::File::open(path).with_context(|| format!("abrindo {}", path.display()))?;
    let mut z = zip::ZipArchive::new(f).context("arquivo zip/ooxml inválido")?;
    let mut e = z
        .by_name(name)
        .with_context(|| format!("{name} não encontrado no arquivo"))?;
    let mut s = String::new();
    e.read_to_string(&mut s)?;
    Ok(s)
}

/// Generic XML → text: `text_tags` contribute text, `para_tags` end lines.
fn xml_text(
    xml: &str,
    text_tags: &[&[u8]],
    para_tags: &[&[u8]],
    tab_tags: &[&[u8]],
    cell_tags: &[&[u8]],
) -> String {
    let mut r = Reader::from_str(xml);
    let mut out = String::new();
    let mut in_text = 0usize;
    loop {
        match r.read_event() {
            Ok(Event::Start(e)) => {
                if text_tags.contains(&e.name().as_ref()) {
                    in_text += 1;
                }
            }
            Ok(Event::Empty(e)) => {
                let n = e.name();
                if tab_tags.contains(&n.as_ref()) {
                    out.push('\t');
                } else if para_tags.contains(&n.as_ref())
                    || n.as_ref() == b"w:br"
                    || n.as_ref() == b"a:br"
                    || n.as_ref() == b"text:line-break"
                {
                    out.push('\n');
                }
            }
            Ok(Event::End(e)) => {
                let n = e.name();
                if text_tags.contains(&n.as_ref()) {
                    in_text = in_text.saturating_sub(1);
                } else if para_tags.contains(&n.as_ref()) {
                    out.push('\n');
                } else if cell_tags.contains(&n.as_ref()) {
                    out.push_str(" | ");
                }
            }
            Ok(Event::Text(t)) if in_text > 0 => {
                let raw = String::from_utf8_lossy(t.as_ref()).into_owned();
                out.push_str(
                    &quick_xml::escape::unescape(&raw)
                        .map(|c| c.into_owned())
                        .unwrap_or(raw),
                );
            }
            Ok(Event::GeneralRef(g)) if in_text > 0 => {
                let name = String::from_utf8_lossy(g.as_ref()).into_owned();
                out.push_str(match name.as_str() {
                    "amp" => "&",
                    "lt" => "<",
                    "gt" => ">",
                    "quot" => "\"",
                    "apos" => "'",
                    _ => "",
                });
                if let Some(code) = name.strip_prefix('#') {
                    let n = code
                        .strip_prefix('x')
                        .map(|h| u32::from_str_radix(h, 16).ok())
                        .unwrap_or_else(|| code.parse().ok());
                    if let Some(c) = n.and_then(char::from_u32) {
                        out.push(c);
                    }
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    out.replace(" | \n", "\n")
}

pub fn docx_text(path: &Path) -> Result<String> {
    let xml = zip_entry(path, "word/document.xml")?;
    Ok(xml_text(
        &xml,
        &[b"w:t"],
        &[b"w:p"],
        &[b"w:tab"],
        &[b"w:tc"],
    ))
}

pub fn odt_text(path: &Path) -> Result<String> {
    let xml = zip_entry(path, "content.xml")?;
    Ok(xml_text(
        &xml,
        &[b"text:p", b"text:h", b"text:span"],
        &[b"text:p", b"text:h"],
        &[b"text:tab"],
        &[b"table:table-cell"],
    ))
}

pub fn pptx_text(path: &Path) -> Result<String> {
    let f = std::fs::File::open(path)?;
    let mut z = zip::ZipArchive::new(f).context("pptx inválido")?;
    let mut slides: Vec<(u32, String)> = Vec::new();
    for i in 0..z.len() {
        let name = z.by_index(i)?.name()?.to_string();
        if let Some(n) = name
            .strip_prefix("ppt/slides/slide")
            .and_then(|r| r.strip_suffix(".xml"))
            .and_then(|n| n.parse().ok())
        {
            slides.push((n, name));
        }
    }
    slides.sort();
    let mut out = String::new();
    for (n, name) in slides {
        let mut s = String::new();
        z.by_name(&name)?.read_to_string(&mut s)?;
        out.push_str(&format!("--- Slide {n} ---\n"));
        out.push_str(xml_text(&s, &[b"a:t"], &[b"a:p"], &[], &[]).trim());
        out.push_str("\n\n");
    }
    Ok(out)
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Runs with **bold** support.
fn runs(text: &str) -> String {
    let mut out = String::new();
    for (i, part) in text.split("**").enumerate() {
        if part.is_empty() {
            continue;
        }
        let bold = if i % 2 == 1 {
            "<w:rPr><w:b/></w:rPr>"
        } else {
            ""
        };
        out.push_str(&format!(
            "<w:r>{bold}<w:t xml:space=\"preserve\">{}</w:t></w:r>",
            esc(part)
        ));
    }
    out
}

/// Minimal but valid DOCX from Markdown-ish text (headings, bullets, paragraphs, bold).
pub fn write_docx(path: &Path, title: Option<&str>, markdown: &str) -> Result<()> {
    let mut body = String::new();
    let para = |style: Option<&str>, text: &str| {
        let ppr = style
            .map(|s| format!("<w:pPr><w:pStyle w:val=\"{s}\"/></w:pPr>"))
            .unwrap_or_default();
        format!("<w:p>{ppr}{}</w:p>", runs(text))
    };
    if let Some(t) = title {
        body.push_str(&para(Some("Title"), t));
    }
    let mut in_code = false;
    for line in markdown.lines() {
        if line.trim_start().starts_with("```") {
            in_code = !in_code;
            continue;
        }
        if in_code {
            body.push_str(&format!("<w:p><w:pPr><w:pStyle w:val=\"Code\"/></w:pPr><w:r><w:t xml:space=\"preserve\">{}</w:t></w:r></w:p>", esc(line)));
            continue;
        }
        let l = line.trim_end();
        if l.trim().is_empty() {
            continue;
        }
        body.push_str(&if let Some(t) = l.strip_prefix("### ") {
            para(Some("Heading3"), t)
        } else if let Some(t) = l.strip_prefix("## ") {
            para(Some("Heading2"), t)
        } else if let Some(t) = l.strip_prefix("# ") {
            para(Some("Heading1"), t)
        } else if let Some(t) = l
            .trim_start()
            .strip_prefix("- ")
            .or_else(|| l.trim_start().strip_prefix("* "))
        {
            para(Some("ListBullet"), &format!("• {t}"))
        } else {
            para(None, l)
        });
    }
    let document = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body}<w:sectPr><w:pgSz w:w="11906" w:h="16838"/><w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440" w:header="708" w:footer="708" w:gutter="0"/></w:sectPr></w:body></w:document>"#
    );
    let style = |id: &str, name: &str, size: u32, bold: bool| {
        format!(
            r#"<w:style w:type="paragraph" w:styleId="{id}"><w:name w:val="{name}"/><w:basedOn w:val="Normal"/><w:next w:val="Normal"/><w:qFormat/><w:pPr><w:spacing w:before="240" w:after="120"/></w:pPr><w:rPr>{}<w:sz w:val="{size}"/></w:rPr></w:style>"#,
            if bold { "<w:b/>" } else { "" }
        )
    };
    let styles = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:docDefaults><w:rPrDefault><w:rPr><w:rFonts w:ascii="Liberation Sans" w:hAnsi="Liberation Sans" w:cs="Liberation Sans"/><w:sz w:val="22"/><w:lang w:val="pt-BR"/></w:rPr></w:rPrDefault><w:pPrDefault><w:pPr><w:spacing w:after="120" w:line="276" w:lineRule="auto"/></w:pPr></w:pPrDefault></w:docDefaults><w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/><w:qFormat/></w:style>{}{}{}{}{}<w:style w:type="paragraph" w:styleId="Code"><w:name w:val="Code"/><w:basedOn w:val="Normal"/><w:rPr><w:rFonts w:ascii="Liberation Mono" w:hAnsi="Liberation Mono"/><w:sz w:val="19"/></w:rPr></w:style></w:styles>"#,
        style("Title", "Title", 40, true),
        style("Heading1", "heading 1", 32, true),
        style("Heading2", "heading 2", 26, true),
        style("Heading3", "heading 3", 23, true),
        style("ListBullet", "List Bullet", 22, false),
    );
    let files: [(&str, String); 5] = [
        (
            "[Content_Types].xml",
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/><Override PartName="/docProps/core.xml" ContentType="application/vnd.openxmlformats-package.core-properties+xml"/></Types>"#.into(),
        ),
        (
            "_rels/.rels",
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties" Target="docProps/core.xml"/></Relationships>"#.into(),
        ),
        (
            "word/_rels/document.xml.rels",
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/></Relationships>"#.into(),
        ),
        (
            "docProps/core.xml",
            format!(
                r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:dcterms="http://purl.org/dc/terms/" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance"><dc:title>{}</dc:title><dc:creator>Jarvis Core</dc:creator><dcterms:created xsi:type="dcterms:W3CDTF">{}</dcterms:created></cp:coreProperties>"#,
                esc(title.unwrap_or("")),
                chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ")
            ),
        ),
        ("word/styles.xml", styles),
    ];
    let f = std::fs::File::create(path)?;
    let mut z = zip::ZipWriter::new(f);
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for (name, content) in files.iter() {
        z.start_file(*name, opts)?;
        z.write_all(content.as_bytes())?;
    }
    z.start_file("word/document.xml", opts)?;
    z.write_all(document.as_bytes())?;
    z.finish()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn docx_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.docx");
        write_docx(
            &p,
            Some("Relatório"),
            "# Introdução\nTexto **forte** & <especial>.\n- item um\n",
        )
        .unwrap();
        let t = docx_text(&p).unwrap();
        assert!(t.contains("Relatório"));
        assert!(t.contains("Texto forte & <especial>."), "{t}");
        assert!(t.contains("• item um"));
    }
}
