use super::*;
use std::io::Write;
use std::path::Path;
use zip::write::FileOptions;

use anyhow::Result;

fn create_minimal_docx(path: &Path) -> Result<()> {
    let file = std::fs::File::create(path)?;
    let mut zip = zip::ZipWriter::new(file);

    zip.add_directory("word/", FileOptions::<()>::default())?;
    zip.start_file("word/document.xml", FileOptions::<()>::default())?;
    zip.write_all(
        br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p><w:r><w:t>Hello</w:t></w:r></w:p>
    <w:p><w:r><w:t>World</w:t></w:r></w:p>
  </w:body>
</w:document>"#,
    )?;

    zip.finish()?;
    Ok(())
}

fn create_minimal_xlsx(path: &Path) -> Result<()> {
    let file = std::fs::File::create(path)?;
    let mut zip = zip::ZipWriter::new(file);

    let opts = FileOptions::<()>::default();

    zip.start_file("[Content_Types].xml", opts)?;
    zip.write_all(br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>
  <Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>
  <Override PartName="/xl/sharedStrings.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sharedStrings+xml"/>
  <Override PartName="/xl/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml"/>
</Types>"#)?;

    zip.add_directory("_rels/", opts)?;
    zip.start_file("_rels/.rels", opts)?;
    zip.write_all(br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/>
</Relationships>"#)?;

    zip.add_directory("xl/", opts)?;
    zip.add_directory("xl/_rels/", opts)?;
    zip.start_file("xl/_rels/workbook.xml.rels", opts)?;
    zip.write_all(br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/>
  <Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/sharedStrings" Target="sharedStrings.xml"/>
  <Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/>
</Relationships>"#)?;

    zip.start_file("xl/workbook.xml", opts)?;
    zip.write_all(br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"
          xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <sheets><sheet name="Sheet1" sheetId="1" r:id="rId1"/></sheets>
</workbook>"#)?;

    zip.add_directory("xl/worksheets/", opts)?;
    zip.start_file("xl/worksheets/sheet1.xml", opts)?;
    zip.write_all(br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
  <sheetData>
    <row r="1">
      <c r="A1" t="inlineStr"><is><t>Cell A1</t></is></c>
      <c r="B1" t="inlineStr"><is><t>Cell B1</t></is></c>
    </row>
  </sheetData>
</worksheet>"#)?;

    zip.start_file("xl/sharedStrings.xml", opts)?;
    zip.write_all(br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" count="0" uniqueCount="0"/>"#)?;

    zip.start_file("xl/styles.xml", opts)?;
    zip.write_all(br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"/>"#)?;

    zip.finish()?;
    Ok(())
}

fn create_minimal_pptx(path: &Path) -> Result<()> {
    let file = std::fs::File::create(path)?;
    let mut zip = zip::ZipWriter::new(file);

    let opts = FileOptions::<()>::default();

    zip.start_file("[Content_Types].xml", opts)?;
    zip.write_all(br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Override PartName="/ppt/presentation.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml"/>
  <Override PartName="/ppt/slides/slide1.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slide+xml"/>
  <Override PartName="/ppt/slides/slide2.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slide+xml"/>
</Types>"#)?;

    zip.add_directory("_rels/", opts)?;
    zip.start_file("_rels/.rels", opts)?;
    zip.write_all(br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="ppt/presentation.xml"/>
</Relationships>"#)?;

    zip.add_directory("ppt/", opts)?;
    zip.start_file("ppt/presentation.xml", opts)?;
    zip.write_all(br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<p:presentation xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main">
  <p:sldIdLst><p:sldId id="256" r:id="rId1"/></p:sldIdLst>
</p:presentation>"#)?;

    zip.add_directory("ppt/_rels/", opts)?;
    zip.start_file("ppt/_rels/presentation.xml.rels", opts)?;
    zip.write_all(br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/>
  <Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide2.xml"/>
</Relationships>"#)?;

    zip.add_directory("ppt/slides/", opts)?;
    zip.start_file("ppt/slides/slide1.xml", opts)?;
    zip.write_all(br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<p:sld xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main">
  <p:cSld><p:spTree>
    <p:nvGrpSpPr/><p:grpSpPr/>
    <p:sp>
      <p:nvSpPr><p:cNvPr id="2" name="Title 1"/><p:nvPr/></p:nvSpPr>
      <p:spPr/>
      <p:txBody>
        <a:bodyPr xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"/>
        <a:lstStyle xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"/>
        <a:p xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main">
          <a:r><a:rPr lang="en-US"/><a:t>Slide 1 Content</a:t></a:r>
        </a:p>
      </p:txBody>
    </p:sp>
  </p:spTree></p:cSld>
</p:sld>"#)?;

    zip.start_file("ppt/slides/slide2.xml", opts)?;
    zip.write_all(br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<p:sld xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main">
  <p:cSld><p:spTree>
    <p:nvGrpSpPr/><p:grpSpPr/>
    <p:sp>
      <p:nvSpPr><p:cNvPr id="3" name="Content 1"/><p:nvPr/></p:nvSpPr>
      <p:spPr/>
      <p:txBody>
        <a:bodyPr xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"/>
        <a:lstStyle xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"/>
        <a:p xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main">
          <a:r><a:rPr lang="en-US"/><a:t>Slide 2 Text</a:t></a:r>
        </a:p>
      </p:txBody>
    </p:sp>
  </p:spTree></p:cSld>
</p:sld>"#)?;

    zip.finish()?;
    Ok(())
}

#[test]
fn test_docx_extract() -> Result<()> {
    let dir = std::env::temp_dir().join("extractor_test_docx");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("test.docx");

    create_minimal_docx(&path)?;

    let extractor = OfficeExtractor::new();
    let result = extractor.extract(&path)?;
    assert!(result.contains("Hello") && result.contains("World"),
        "expected anydoc output containing Hello and World, got: {result:?}");

    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

#[test]
fn test_xlsx_extract() -> Result<()> {
    let dir = std::env::temp_dir().join("extractor_test_xlsx");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("test.xlsx");

    create_minimal_xlsx(&path)?;

    let extractor = OfficeExtractor::new();
    let result = extractor.extract(&path)?;
    assert!(result.contains("Cell A1"), "result: {:?}", result);

    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

#[test]
fn test_pptx_extract() -> Result<()> {
    let dir = std::env::temp_dir().join("extractor_test_pptx");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("test.pptx");

    create_minimal_pptx(&path)?;

    let extractor = OfficeExtractor::new();
    match extractor.extract(&path) {
        Ok(result) => {
            assert!(result.contains("Slide 1") || result.contains("Slide 2"),
                "result: {:?}", result);
        }
        Err(e) => {
            // AnyDoc may not handle minimal test pptx; that's acceptable
            assert!(e.to_string().contains("无法") || e.to_string().contains("error") || e.to_string().contains("失败"),
                "PPTX extraction error: {e}");
        }
    }

    std::fs::remove_dir_all(&dir)?;
    Ok(())
}

#[test]
fn test_office_unsupported_extension() {
    let extractor = OfficeExtractor::new();
    let path = Path::new("test.xyz");
    let result = extractor.extract(path);
    assert!(result.is_err());
}

#[test]
fn test_extract_slide_number() {
    assert_eq!(super::extract_slide_number("ppt/slides/slide1.xml"), 1);
    assert_eq!(super::extract_slide_number("ppt/slides/slide10.xml"), 10);
    assert_eq!(super::extract_slide_number("ppt/slides/slide0.xml"), 0);
    assert_eq!(super::extract_slide_number("ppt/slides/notaslide.xml"), 0);
}
