use api::AppState;
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use serde_json::{Value, json};
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};
use tower::ServiceExt;

const TOKEN: &str = "document-extraction-test";
static SERIAL: AtomicU64 = AtomicU64::new(0);

struct Directory(PathBuf);
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

async fn app() -> (Directory, Router) {
    let directory = Directory(std::env::temp_dir().join(format!(
        "open-harness-document-api-{}-{}",
        std::process::id(),
        SERIAL.fetch_add(1, Ordering::Relaxed),
    )));
    let state = AppState::open_with_delay(directory.0.clone(), Some(TOKEN.into()), Duration::ZERO)
        .await
        .unwrap();
    (directory, api::app(state))
}

async fn request(
    app: Router,
    path: &str,
    bytes: Vec<u8>,
    token: Option<&str>,
    origin: Option<&str>,
) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/octet-stream");
    if let Some(token) = token {
        request = request.header("authorization", format!("Bearer {token}"));
    }
    if let Some(origin) = origin {
        request = request.header("origin", origin);
    }
    let response = app
        .oneshot(request.body(Body::from(bytes)).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 2 * 1024 * 1024)
        .await
        .unwrap();
    let value =
        serde_json::from_slice(&bytes).unwrap_or_else(|_| json!(String::from_utf8_lossy(&bytes)));
    (status, value)
}

async fn extract(app: Router, name: &str, bytes: Vec<u8>) -> (StatusCode, Value) {
    request(
        app,
        &format!("/api/documents/extract?name={name}"),
        bytes,
        Some(TOKEN),
        None,
    )
    .await
}

// All document bytes are synthesized in the tests. No user files or external
// parser/renderer processes are needed to exercise this upload boundary.
fn pdf(text: Option<&str>) -> Vec<u8> {
    let content = text.map_or_else(String::new, |text| {
        let text = text
            .replace('\\', "\\\\")
            .replace('(', "\\(")
            .replace(')', "\\)");
        format!("BT /F1 12 Tf 20 100 Td ({text}) Tj ET")
    });
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>".to_owned(),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_owned(),
        format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()),
    ];
    let mut pdf = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (index, object) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.extend_from_slice(format!("{} 0 obj\n{object}\nendobj\n", index + 1).as_bytes());
    }
    let start = pdf.len();
    pdf.extend_from_slice(b"xref\n0 6\n0000000000 65535 f \n");
    for offset in offsets {
        pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(
        format!("trailer\n<< /Root 1 0 R /Size 6 >>\nstartxref\n{start}\n%%EOF\n").as_bytes(),
    );
    pdf
}

fn docx(text: &str) -> Vec<u8> {
    let text = text
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    office_xml(&format!("<document><p><t>{text}</t></p></document>"))
}

fn office_xml(xml: &str) -> Vec<u8> {
    // A minimal stored ZIP entry keeps synthetic fixtures dependency-free.
    let name = b"word/document.xml";
    let bytes = xml.as_bytes();
    let mut crc = u32::MAX;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & 0u32.wrapping_sub(crc & 1));
        }
    }
    let crc = !crc;
    let size = u32::try_from(bytes.len()).unwrap();
    let mut zip = 0x0403_4b50u32.to_le_bytes().to_vec();
    for field in [20u16, 0, 0, 0, 0] {
        zip.extend_from_slice(&field.to_le_bytes());
    }
    for field in [crc, size, size] {
        zip.extend_from_slice(&field.to_le_bytes());
    }
    zip.extend_from_slice(&(name.len() as u16).to_le_bytes());
    zip.extend_from_slice(&0u16.to_le_bytes());
    zip.extend_from_slice(name);
    zip.extend_from_slice(bytes);
    let offset = zip.len() as u32;
    zip.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
    for field in [20u16, 20, 0, 0, 0, 0] {
        zip.extend_from_slice(&field.to_le_bytes());
    }
    for field in [crc, size, size] {
        zip.extend_from_slice(&field.to_le_bytes());
    }
    for field in [name.len() as u16, 0, 0, 0, 0] {
        zip.extend_from_slice(&field.to_le_bytes());
    }
    zip.extend_from_slice(&[0; 8]);
    zip.extend_from_slice(name);
    let central_size = zip.len() as u32 - offset;
    zip.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    for field in [0u16, 0, 1, 1] {
        zip.extend_from_slice(&field.to_le_bytes());
    }
    zip.extend_from_slice(&central_size.to_le_bytes());
    zip.extend_from_slice(&offset.to_le_bytes());
    zip.extend_from_slice(&0u16.to_le_bytes());
    zip
}

#[tokio::test]
async fn extracts_pdf_and_docx_into_snapshots_with_locations_and_warnings() {
    let (_directory, app) = app().await;
    for (name, bytes, expected, location, warning) in [
        (
            "notes.PDF",
            pdf(Some("Synthetic PDF text")),
            "Synthetic PDF text",
            "[page 1]",
            "images and diagrams",
        ),
        (
            "notes.docx",
            docx("合成文档 & 内容"),
            "合成文档 & 内容",
            "[paragraph 1]",
            "headers, footers",
        ),
    ] {
        let (status, result) = extract(app.clone(), name, bytes).await;
        assert_eq!(status, StatusCode::OK, "{result}");
        let text = result["text"].as_str().unwrap();
        assert!(text.contains(expected), "{text}");
        assert!(text.contains(location), "{text}");
        assert!(text.contains(warning), "{text}");
        assert_eq!(result.as_object().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn refuses_missing_credentials_and_browser_origins_before_extraction() {
    let (_directory, app) = app().await;
    for (token, origin, expected) in [
        (None, None, StatusCode::UNAUTHORIZED),
        (Some("incorrect"), None, StatusCode::UNAUTHORIZED),
        (
            Some(TOKEN),
            Some("https://untrusted.example"),
            StatusCode::FORBIDDEN,
        ),
    ] {
        let (status, _) = request(
            app.clone(),
            "/api/documents/extract?name=file.pdf",
            pdf(Some("text")),
            token,
            origin,
        )
        .await;
        assert_eq!(status, expected);
    }
}

#[tokio::test]
async fn accepts_bytes_only_with_a_supported_basename() {
    let (_directory, app) = app().await;
    for name in [
        "",
        "..%2Fsecret.pdf",
        "%2Ftmp%2Fsecret.pdf",
        "dir%5Csecret.pdf",
        "bad%00.pdf",
        "text.txt",
        "old.doc",
        "archive.zip",
    ] {
        let (status, _) = extract(app.clone(), name, pdf(Some("text"))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{name}");
    }
    let (status, _) = request(
        app.clone(),
        "/api/documents/extract?name=file.pdf&path=%2Ftmp%2Fsecret.pdf",
        pdf(Some("text")),
        Some(TOKEN),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    for (name, bytes) in [
        ("broken.pdf", b"not a PDF".to_vec()),
        ("broken.docx", b"not a ZIP".to_vec()),
        ("locked.docx", vec![0xd0, 0xcf, 0x11, 0xe0]),
    ] {
        let (status, result) = extract(app.clone(), name, bytes).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(
            result["error"]
                .as_str()
                .unwrap()
                .contains("无法提取文档文字")
        );
    }
}

#[tokio::test]
async fn rejects_empty_documents_and_text_over_the_message_snapshot_budget() {
    let (_directory, app) = app().await;
    for (name, bytes) in [("scan.pdf", pdf(None)), ("empty.docx", docx(" \n\t"))] {
        let (status, result) = extract(app.clone(), name, bytes).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(
            result["error"]
                .as_str()
                .unwrap()
                .contains("没有可提取的正文")
        );
        assert!(result["error"].as_str().unwrap().contains("OCR"));
    }
    // The text itself fits exactly, but location and extraction warnings count too.
    let (status, result) = extract(app.clone(), "large.docx", docx(&"x".repeat(256 * 1024))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(result["error"].as_str().unwrap().contains("256 KiB"));
    // This bound measures UTF-8 bytes, not characters.
    let (status, result) = extract(app, "unicode.docx", docx(&"汉".repeat(90_000))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(result["error"].as_str().unwrap().contains("256 KiB"));
}

#[tokio::test]
async fn normalizes_form_feed_and_refuses_other_control_characters() {
    let (_directory, app) = app().await;
    let (status, result) = extract(app.clone(), "pages.docx", docx("first\u{c}second")).await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert!(result["text"].as_str().unwrap().contains("first\nsecond"));
    for control in ['\0', '\u{1}', '\u{85}'] {
        let (status, result) =
            extract(app.clone(), "control.docx", docx(&format!("a{control}b"))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(
            result["error"].as_str().unwrap().contains("控制字符"),
            "{result}"
        );
    }
}

#[tokio::test]
async fn document_upload_has_a_separate_bounded_body_limit() {
    let (_directory, app) = app().await;
    let xml = format!(
        "<document><!--{}--><p><t>Small extracted text</t></p></document>",
        "x".repeat(4 * 1024 * 1024)
    );
    let document = office_xml(&xml);
    assert!(document.len() > 4 * 1024 * 1024);
    let (status, result) = extract(app.clone(), "source.docx", document).await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert!(
        result["text"]
            .as_str()
            .unwrap()
            .contains("Small extracted text")
    );
    let (status, result) = extract(
        app.clone(),
        "oversize.pdf",
        vec![b' '; 32 * 1024 * 1024 + 1],
    )
    .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{result}");
    assert!(result["error"].as_str().unwrap().contains("32 MiB"));

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/models")
                .header("authorization", format!("Bearer {TOKEN}"))
                .header("content-type", "application/json")
                .body(Body::from(vec![b' '; 4 * 1024 * 1024 + 1]))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
}
