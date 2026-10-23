use agent_local::{LocalTools, PermissionMode};
use agent_runtime::{ToolCall, ToolExecutor};
use serde_json::{Value, json};
use std::{
    fs,
    io::{Cursor, Write},
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

struct Workspace {
    base: PathBuf,
    root: PathBuf,
    tools: LocalTools,
}

impl Workspace {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let base = std::env::temp_dir().join(format!(
            "harness-search-pages-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let root = base.join("project");
        fs::create_dir_all(&root).unwrap();
        let tools =
            LocalTools::open(root.clone(), base.join("state"), PermissionMode::ReadOnly).unwrap();
        Self { base, root, tools }
    }

    async fn call(&self, name: &str, arguments: Value) -> Value {
        let output = self
            .tools
            .execute(ToolCall {
                id: "pagination".into(),
                name: name.into(),
                arguments: arguments.to_string(),
            })
            .await
            .unwrap();
        assert!(output.len() <= 64 * 1024);
        serde_json::from_str(&output).unwrap()
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.base);
    }
}

#[tokio::test]
async fn escaped_search_results_paginate_without_losing_matches() {
    let workspace = Workspace::new();
    let content = (0..150)
        .map(|index| format!("{}needle {index}\n", "\u{1}".repeat(200)))
        .collect::<String>();
    fs::write(workspace.root.join("matches.txt"), content).unwrap();
    let mut offset = 0;
    let mut lines = Vec::new();
    loop {
        let page = workspace
            .call(
                "search_files",
                json!({"path":"matches.txt", "query":"needle", "offset":offset}),
            )
            .await;
        let matches = page["matches"].as_array().unwrap();
        assert!(!matches.is_empty());
        assert!(matches.len() < 100); // JSON escaping, not the item cap, bounds this page.
        lines.extend(matches.iter().map(|entry| entry["line"].as_u64().unwrap()));
        let Some(next) = page["next_offset"].as_u64() else {
            assert_eq!(page["coverage"]["complete"], true);
            break;
        };
        assert_eq!(next, offset + matches.len() as u64);
        assert_eq!(page["coverage"]["complete"], false);
        offset = next;
    }
    assert_eq!(lines, (1..=150).collect::<Vec<_>>());
}

fn workbook_with_long_locations() -> Vec<u8> {
    fn entry(archive: &mut zip::ZipWriter<Cursor<Vec<u8>>>, name: &str, data: &str) {
        archive
            .start_file(
                name,
                zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Deflated),
            )
            .unwrap();
        archive.write_all(data.as_bytes()).unwrap();
    }
    let mut archive = zip::ZipWriter::new(Cursor::new(Vec::new()));
    entry(
        &mut archive,
        "_rels/.rels",
        "<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"xl/workbook.xml\"/></Relationships>",
    );
    entry(
        &mut archive,
        "[Content_Types].xml",
        "<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"><Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/><Default Extension=\"xml\" ContentType=\"application/xml\"/><Override PartName=\"/xl/workbook.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml\"/></Types>",
    );
    let mut sheets = String::new();
    let mut relationships = String::new();
    for index in 1..=40 {
        // Extraction accepts hostile/overlong metadata too. Its display must not
        // consume the complete output budget or alter the text pagination cursor.
        sheets.push_str(&format!(
            "<sheet name=\"sheet {index} {}\" sheetId=\"{index}\" r:id=\"rId{index}\"/>",
            "\\".repeat(1800)
        ));
        relationships.push_str(&format!(
            "<Relationship Id=\"rId{index}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet\" Target=\"worksheets/sheet{index}.xml\"/>"
        ));
        entry(
            &mut archive,
            &format!("xl/worksheets/sheet{index}.xml"),
            &format!(
                "<worksheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\"><sheetData><row r=\"1\"><c r=\"A1\" t=\"inlineStr\"><is><t>needle {index}</t></is></c></row></sheetData></worksheet>"
            ),
        );
    }
    entry(
        &mut archive,
        "xl/workbook.xml",
        &format!(
            "<workbook xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"><sheets>{sheets}</sheets></workbook>"
        ),
    );
    entry(
        &mut archive,
        "xl/_rels/workbook.xml.rels",
        &format!(
            "<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">{relationships}</Relationships>"
        ),
    );
    archive.finish().unwrap().into_inner()
}

#[tokio::test]
async fn document_pages_bound_locations_and_resume_at_unreturned_units() {
    let workspace = Workspace::new();
    fs::write(
        workspace.root.join("locations.xlsx"),
        workbook_with_long_locations(),
    )
    .unwrap();
    let mut arguments = json!({"path":"locations.xlsx"});
    let mut units = Vec::new();
    loop {
        let page = workspace.call("read_file", arguments).await;
        let chunks = page["chunks"].as_array().unwrap();
        assert!(!chunks.is_empty());
        assert!(chunks.len() < 32);
        for chunk in chunks {
            let unit = chunk["unit"].as_u64().unwrap();
            assert_eq!(chunk["text"], format!("A1: needle {unit}\n"));
            assert!(chunk["location"].as_str().unwrap().len() <= 1024);
            units.push(unit);
        }
        if page["next_cursor"].is_null() {
            break;
        }
        assert_eq!(page["next_cursor"]["unit"], units.last().unwrap() + 1);
        arguments = json!({"path":"locations.xlsx", "cursor":page["next_cursor"]});
    }
    assert_eq!(units, (1..=40).collect::<Vec<_>>());

    let mut offset = 0;
    let mut found = Vec::new();
    loop {
        let page = workspace
            .call(
                "search_files",
                json!({"path":"locations.xlsx", "query":"needle", "offset":offset}),
            )
            .await;
        let matches = page["matches"].as_array().unwrap();
        assert!(!matches.is_empty());
        assert!(matches.len() < 40);
        found.extend(matches.iter().map(|entry| entry["unit"].as_u64().unwrap()));
        let Some(next) = page["next_offset"].as_u64() else {
            break;
        };
        assert_eq!(next, offset + matches.len() as u64);
        offset = next;
    }
    assert_eq!(found, units);
}

#[tokio::test]
async fn directory_search_reports_skipped_documents_and_binary_content() {
    let workspace = Workspace::new();
    fs::write(workspace.root.join("found.txt"), "needle").unwrap();
    fs::write(
        workspace.root.join("manual.pdf"),
        b"%PDF-not-parsed-in-directory-search",
    )
    .unwrap();
    fs::write(workspace.root.join("image.bin"), [0, 1, 2]).unwrap();
    fs::write(workspace.root.join(".env.private"), "needle secret").unwrap();
    let page = workspace
        .call("search_files", json!({"path":".","query":"needle"}))
        .await;
    assert_eq!(page["matches"].as_array().unwrap().len(), 1);
    assert_eq!(page["matches"][0]["path"], "found.txt");
    assert_eq!(page["coverage"]["complete"], false);
    assert_eq!(page["coverage"]["skipped_count"], 3);
    let skipped = page["coverage"]["skipped"].as_array().unwrap();
    assert!(skipped.iter().any(|entry| entry["path"] == "manual.pdf"
        && entry["reason"].as_str().unwrap().contains("search_files")));
    assert!(skipped.iter().any(|entry| entry["path"] == "image.bin"));
    assert!(skipped.iter().any(|entry| entry["path"] == ".env.private"));
}

#[tokio::test]
async fn excluded_entries_still_report_directory_enumeration_limits() {
    let workspace = Workspace::new();
    for index in 0..16_386 {
        fs::write(workspace.root.join(format!(".env.{index}")), "").unwrap();
    }
    let page = workspace
        .call("search_files", json!({"path":".","query":"needle"}))
        .await;
    assert_eq!(page["limit_reached"], true);
    assert_eq!(page["coverage"]["complete"], false);
    assert!(page["matches"].as_array().unwrap().is_empty());
    assert!(
        page["coverage"]["skipped"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["reason"] == "directory enumeration limit reached")
    );
}

#[cfg(unix)]
#[tokio::test]
async fn unreadable_subdirectories_do_not_discard_other_matches() {
    use std::os::unix::fs::PermissionsExt;
    // Root may read mode-000 directories, so this fixture is meaningful only
    // when filesystem permissions are enforced for the test process.
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    let workspace = Workspace::new();
    fs::write(workspace.root.join("found.txt"), "needle").unwrap();
    let inaccessible = workspace.root.join("unreadable");
    fs::create_dir(&inaccessible).unwrap();
    fs::set_permissions(&inaccessible, fs::Permissions::from_mode(0o000)).unwrap();
    let result = workspace
        .tools
        .execute(ToolCall {
            id: "unreadable".into(),
            name: "search_files".into(),
            arguments: json!({"path":".", "query":"needle"}).to_string(),
        })
        .await;
    fs::set_permissions(&inaccessible, fs::Permissions::from_mode(0o700)).unwrap();
    let page: Value = serde_json::from_str(&result.unwrap()).unwrap();
    assert_eq!(page["matches"][0]["path"], "found.txt");
    assert_eq!(page["coverage"]["complete"], false);
    assert_eq!(page["coverage"]["skipped_count"], 1);
    assert_eq!(
        page["coverage"]["skipped"][0]["reason"],
        "directory unavailable"
    );
}
