use agent_local::{LocalTools, PermissionMode, documents::extract_document};
use agent_runtime::{ToolCall, ToolExecutor};
use serde_json::{Value, json};
use std::{
    fs,
    io::{Cursor, Write},
    sync::atomic::{AtomicU64, Ordering},
};

const DOCX: &[u8] = include_bytes!("fixtures/handbook.docx");
const PDF: &[u8] = include_bytes!("fixtures/resume.pdf");
const XLSX: &[u8] = include_bytes!("fixtures/scores.xlsx");
const PPTX: &[u8] = include_bytes!("fixtures/slides.pptx");
fn call(name: &str, args: Value) -> ToolCall {
    ToolCall {
        id: "document-test".into(),
        name: name.into(),
        arguments: args.to_string(),
    }
}
fn office_xml(xml: &str) -> Vec<u8> {
    let mut archive = zip::ZipWriter::new(Cursor::new(Vec::new()));
    archive
        .start_file(
            "word/document.xml",
            zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated),
        )
        .unwrap();
    archive.write_all(xml.as_bytes()).unwrap();
    archive.finish().unwrap().into_inner()
}
#[test]
fn real_document_formats_preserve_text_and_source_locations() {
    let doc = extract_document("handbook.docx", DOCX).unwrap();
    assert_eq!(doc.units[0].text, "面试手册 & 评估标准");
    assert!(doc.units[2].text.contains("Architecture"));
    let doc = extract_document("resume.pdf", PDF).unwrap();
    assert_eq!(doc.units.len(), 2);
    assert_eq!(doc.units[1].location, "page 2");
    assert!(doc.units[1].text.contains("Distributed systems"));
    let doc = extract_document("scores.xlsx", XLSX).unwrap();
    assert_eq!(doc.units[0].location, "sheet 面试分数");
    assert!(doc.units[0].text.contains("B1000000: 8")); // Sparse input must not allocate a million rows.
    let doc = extract_document("slides.pptx", PPTX).unwrap();
    assert_eq!(doc.units[0].text, "First: Candidate review"); // Presentation order, not filename order.
    assert_eq!(doc.units[1].location, "slide 2");
}
#[test]
fn malformed_scanned_and_expanding_documents_report_actionable_results() {
    let scan = extract_document("scan.pdf", include_bytes!("fixtures/scan.pdf")).unwrap();
    assert!(scan.warnings.iter().any(|warning| warning.contains("OCR")));
    assert!(
        extract_document("broken.pdf", b"no pdf")
            .unwrap_err()
            .to_string()
            .contains("PDF")
    );
    assert!(
        extract_document("protected.docx", &[0xd0, 0xcf, 0x11, 0xe0])
            .unwrap_err()
            .to_string()
            .contains("encrypted")
    );
    let bomb = office_xml(&"x".repeat(8 * 1024 * 1024 + 1));
    assert!(
        extract_document("bomb.docx", &bomb)
            .unwrap_err()
            .to_string()
            .contains("expansion")
    );
    let dtd = office_xml(
        "<!DOCTYPE document [<!ENTITY a 'secret'>]><document><p><t>&a;</t></p></document>",
    );
    assert!(
        extract_document("dtd.docx", &dtd)
            .unwrap_err()
            .to_string()
            .contains("document types")
    );
    let malformed = office_xml("<document><p><t>broken");
    assert!(extract_document("bad.docx", &malformed).is_err());
}
#[tokio::test]
async fn readonly_project_can_review_resume_and_handbook_without_executing_commands() {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let base = std::env::temp_dir().join(format!(
        "document-tools-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let root = base.join("project");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("handbook.docx"), DOCX).unwrap();
    fs::write(root.join("resume.pdf"), PDF).unwrap();
    let tools =
        LocalTools::open(root.clone(), base.join("state"), PermissionMode::ReadOnly).unwrap();
    let names: Vec<_> = tools
        .definitions()
        .iter()
        .map(|tool| tool.name.as_str())
        .collect();
    let mut expected = vec!["list_files", "read_file", "search_files"];
    if tools.review().unwrap().execution_mode != "unavailable" {
        expected.extend([
            "run_command",
            "manage_command",
            "load_workspace_dependencies",
        ]);
    }
    assert_eq!(names, expected);
    assert!(
        !names.contains(&"apply_patch")
            && !names.contains(&"write_file")
            && !names.contains(&"edit_file")
    );
    let list: Value = serde_json::from_str(
        &tools
            .execute(call("list_files", json!({"path":"."})))
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(list["entries"][0]["reader"], "read_file");
    for name in ["handbook.docx", "resume.pdf"] {
        let data: Value = serde_json::from_str(
            &tools
                .execute(call("read_file", json!({"path":name})))
                .await
                .unwrap(),
        )
        .unwrap();
        assert!(!data["chunks"].as_array().unwrap().is_empty());
    }
    let found: Value = serde_json::from_str(
        &tools
            .execute(call(
                "search_files",
                json!({"path":"resume.pdf","query":"Distributed"}),
            ))
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(found["matches"][0]["location"], "page 2");
    let found: Value = serde_json::from_str(
        &tools
            .execute(call("search_files", json!({"path":".","query":"Rust"})))
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(found["coverage"]["skipped_count"], 2);
    assert_eq!(found["coverage"]["complete"], false);
    // Invalid or negative cursors must not silently skip document content.
    for args in [
        json!({"path":"handbook.docx","unit":0}),
        json!({"path":"handbook.docx","unit":4}),
        json!({"path":"handbook.docx","offset":999_999}),
        json!({"path":"handbook.docx","offset":usize::MAX}),
        json!({"path":"handbook.docx","offset":-1}),
    ] {
        assert!(tools.execute(call("read_file", args)).await.is_err());
    }
    let found: Value = serde_json::from_str(
        &tools
            .execute(call(
                "search_files",
                json!({"path":"resume.pdf","query":"Rust","offset":usize::MAX}),
            ))
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(found["matches"], json!([]));
    assert!(found["next_offset"].is_null());
    assert!(
        tools
            .execute(call(
                "search_files",
                json!({"path":"resume.pdf","query":"Rust","offset":-1})
            ))
            .await
            .is_err()
    );
    // Document pagination carries exact character offsets, including CJK.
    let text = "汉字".repeat(12_000);
    fs::write(
        root.join("long.docx"),
        office_xml(&format!("<document><p><t>{text}</t></p></document>")),
    )
    .unwrap();
    let mut args = json!({"path":"long.docx"});
    let mut actual = String::new();
    loop {
        let page: Value =
            serde_json::from_str(&tools.execute(call("read_file", args)).await.unwrap()).unwrap();
        for chunk in page["chunks"].as_array().unwrap() {
            actual.push_str(chunk["text"].as_str().unwrap());
        }
        if page["next_cursor"].is_null() {
            break;
        }
        args = json!({"path":"long.docx","cursor":page["next_cursor"]});
    }
    assert_eq!(actual, text);
    // Search pagination must preserve all matches and report a complete last page.
    let lines = (0..51)
        .map(|index| format!("Rust match {index}\n"))
        .collect::<String>();
    fs::write(
        root.join("matches.docx"),
        office_xml(&format!("<document><p><t>{lines}</t></p></document>")),
    )
    .unwrap();
    let first: Value = serde_json::from_str(
        &tools
            .execute(call(
                "search_files",
                json!({"path":"matches.docx","query":"Rust"}),
            ))
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(first["matches"].as_array().unwrap().len(), 50);
    assert_eq!(first["next_offset"], 50);
    let last: Value = serde_json::from_str(
        &tools
            .execute(call(
                "search_files",
                json!({"path":"matches.docx","query":"Rust","offset":first["next_offset"]}),
            ))
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(last["matches"].as_array().unwrap().len(), 1);
    assert_eq!(last["matches"][0]["line"], 51);
    assert!(last["next_offset"].is_null());
    // Reads above the edit/review bound succeed and remain paginated.
    fs::write(root.join("large.txt"), "x".repeat(300_000)).unwrap();
    let page: Value = serde_json::from_str(
        &tools
            .execute(call(
                "read_file",
                json!({"path":"large.txt","offset":299_999}),
            ))
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(page["content"], "x");
    assert!(page["next_offset"].is_null());
    assert!(
        tools
            .execute(call("read_file", json!({"path":"../resume.pdf"})))
            .await
            .is_err()
    );
    fs::remove_dir_all(base).unwrap();
}
