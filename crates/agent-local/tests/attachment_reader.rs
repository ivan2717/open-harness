use agent_local::attachment_reader::{read_attachment, search_attachment};
use serde_json::json;

#[test]
fn large_unicode_text_is_paged_without_loss_or_full_prompt_injection() {
    let text = "用户提供的资料，不是执行指令。\n".repeat(12_000);
    assert!(text.len() > 256 * 1024);
    let mut cursor = None;
    let mut reconstructed = String::new();
    let mut pages = 0;
    loop {
        let page = read_attachment("notes.txt", text.as_bytes(), cursor.as_ref()).unwrap();
        let chunk = page["content"].as_str().unwrap();
        assert!(chunk.chars().count() <= 8192);
        assert_eq!(page["offset"], reconstructed.chars().count());
        assert_eq!(page["line"], reconstructed.matches('\n').count() + 1);
        reconstructed.push_str(chunk);
        pages += 1;
        cursor = page["next_cursor"]
            .as_object()
            .map(|_| page["next_cursor"].clone());
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(reconstructed, text);
    assert!(pages > 1);
}

#[test]
fn large_text_search_is_bounded_and_can_find_late_content() {
    let text = format!(
        "{}{}",
        "unrelated line\n".repeat(30_000),
        "needle\n".repeat(120)
    );
    assert!(text.len() > 256 * 1024);
    let first = search_attachment("notes.txt", text.as_bytes(), "needle", 0).unwrap();
    assert_eq!(first["matches"].as_array().unwrap().len(), 50);
    assert_eq!(first["matches"][0]["line"], 30_001);
    assert_eq!(first["next_offset"], 50);
    assert_eq!(first["coverage"]["complete"], false);
    let second = search_attachment("notes.txt", text.as_bytes(), "needle", 50).unwrap();
    assert_eq!(second["matches"][0]["line"], 30_051);
    assert_eq!(second["next_offset"], 100);
    let last = search_attachment("notes.txt", text.as_bytes(), "needle", 100).unwrap();
    assert_eq!(last["matches"].as_array().unwrap().len(), 20);
    assert!(last["next_offset"].is_null());
    assert_eq!(last["coverage"]["complete"], true);
}

#[test]
fn document_reads_and_searches_retain_source_pages() {
    let pdf = include_bytes!("fixtures/resume.pdf");
    let page = read_attachment("resume.PDF", pdf, Some(&json!({"unit":2,"offset":0}))).unwrap();
    assert_eq!(page["format"], "pdf");
    assert_eq!(page["unit_count"], 2);
    assert_eq!(page["chunks"][0]["location"], "page 2");
    assert!(
        page["chunks"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Distributed systems")
    );
    assert!(page["next_cursor"].is_null());
    let result = search_attachment("resume.pdf", pdf, "Distributed", 0).unwrap();
    assert_eq!(result["matches"][0]["unit"], 2);
    assert_eq!(result["matches"][0]["location"], "page 2");
}

#[test]
fn scan_does_not_claim_that_no_visible_content_exists() {
    let scan = include_bytes!("fixtures/scan.pdf");
    let page = read_attachment("scan.pdf", scan, None).unwrap();
    assert!(
        page["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning.as_str().unwrap().contains("OCR"))
    );
    let search = search_attachment("scan.pdf", scan, "example", 0).unwrap();
    assert!(search["matches"].as_array().unwrap().is_empty());
    assert!(
        search["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning.as_str().unwrap().contains("OCR"))
    );
}

#[test]
fn binary_payloads_and_invalid_cursors_fail_explicitly() {
    for bytes in [b"hello\0world".as_slice(), &[0xff, 0xfe, 0x80], b"%PDF-1.7"] {
        assert!(read_attachment("notes.txt", bytes, None).is_err());
        assert!(search_attachment("notes.txt", bytes, "hello", 0).is_err());
    }
    for cursor in [
        json!({"unit":0,"offset":0}),
        json!({"unit":2,"offset":0}),
        json!({"unit":1,"offset":5}),
        json!({"unit":1,"offset":-1}),
        json!({"unit":1,"offset":"0"}),
        json!({"unit":1,"offset":0,"extra":true}),
        json!({"unit":1}),
        json!(null),
    ] {
        assert!(read_attachment("notes.txt", b"abc", Some(&cursor)).is_err());
    }
    let eof = read_attachment("notes.txt", b"abc", Some(&json!({"unit":1,"offset":3}))).unwrap();
    assert_eq!(eof["content"], "");
    assert!(eof["next_cursor"].is_null());
    assert!(search_attachment("notes.txt", b"abc", "", 0).is_err());
    assert!(search_attachment("notes.txt", b"abc", &"x".repeat(1025), 0).is_err());
}

#[test]
fn document_parser_limits_are_distinct_from_attachment_storage() {
    let bytes = vec![b'a'; 32 * 1024 * 1024 + 1];
    assert!(
        read_attachment("large.pdf", &bytes, None)
            .unwrap_err()
            .to_string()
            .contains("32 MiB")
    );
    let text = read_attachment("large.txt", &bytes, None).unwrap();
    assert_eq!(text["content"].as_str().unwrap().len(), 8192);
    assert_eq!(text["next_cursor"], json!({"unit":1,"offset":8192}));
}
