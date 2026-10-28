//! Convert explicitly supplied document bytes to a bounded message snapshot.
//! No path is accepted or opened, and extraction never grants workspace access.
use crate::{input, service::ServiceError};
use agent_core::TextFileReference;
use agent_local::documents::{extract_document, supported};
use axum::{
    Json,
    body::Bytes,
    extract::{Query, rejection::BytesRejection},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use std::path::Path;
use tokio::sync::Semaphore;

static EXTRACTION_SLOTS: Semaphore = Semaphore::const_new(2);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DocumentQuery {
    name: String,
}

#[derive(Serialize)]
pub(super) struct DocumentText {
    text: String,
}

pub(super) async fn extract(
    Query(query): Query<DocumentQuery>,
    body: Result<Bytes, BytesRejection>,
) -> Result<Json<DocumentText>, Response> {
    let reference = TextFileReference {
        attachment: None,
        image: None,
        name: query.name,
        path: None,
        text: String::new(),
    };
    input::validate("", std::slice::from_ref(&reference)).map_err(IntoResponse::into_response)?;
    if !supported(Path::new(&reference.name)) {
        return Err(ServiceError::BadRequest(
            "仅支持 PDF、DOCX、XLSX、PPTX 文档，请选择受支持的文件格式".into(),
        )
        .into_response());
    }
    let bytes = body.map_err(|error| {
        let message = if error.status() == StatusCode::PAYLOAD_TOO_LARGE {
            "文档超过 32 MiB，请先拆分或缩小文件"
        } else {
            "无法读取文档内容，请重新添加文件"
        };
        (error.status(), Json(serde_json::json!({"error": message}))).into_response()
    })?;
    let permit = EXTRACTION_SLOTS.acquire().await.map_err(|_| {
        ServiceError::Conflict("文档提取暂不可用，请稍后重试".into()).into_response()
    })?;
    tokio::task::spawn_blocking(move || {
        // Keep the slot occupied until parsing ends, even if the HTTP request
        // is cancelled while this blocking task is still running.
        let _permit = permit;
        snapshot(reference, &bytes)
    })
    .await
    .map_err(|_| {
        ServiceError::BadRequest("文档解析失败，请重新导出后再添加".into()).into_response()
    })?
    .map(Json)
    .map_err(IntoResponse::into_response)
}

fn snapshot(mut reference: TextFileReference, bytes: &[u8]) -> Result<DocumentText, ServiceError> {
    let document = extract_document(&reference.name, bytes)
        .map_err(|error| ServiceError::BadRequest(format!("无法提取文档文字：{error}")))?;
    if document
        .units
        .iter()
        .all(|unit| unit.text.trim().is_empty())
    {
        return Err(ServiceError::BadRequest(
            "文档没有可提取的正文；扫描件或图片文档需要先进行 OCR 文字识别".into(),
        ));
    }
    append(&mut reference.text, "文档文字提取说明：\n")?;
    for warning in document.warnings {
        append(&mut reference.text, "- ")?;
        append(&mut reference.text, &warning)?;
        append(&mut reference.text, "\n")?;
    }
    for unit in document.units {
        append(&mut reference.text, "\n[")?;
        append(&mut reference.text, &unit.location)?;
        append(&mut reference.text, "]\n")?;
        append(&mut reference.text, &unit.text)?;
        append(&mut reference.text, "\n")?;
    }
    input::validate("", std::slice::from_ref(&reference))?;
    Ok(DocumentText {
        text: reference.text,
    })
}

fn append(target: &mut String, value: &str) -> Result<(), ServiceError> {
    for character in value.chars() {
        let character = if character == '\u{c}' {
            '\n'
        } else {
            character
        };
        if character.is_control() && !matches!(character, '\n' | '\r' | '\t') {
            return Err(ServiceError::BadRequest(
                "提取的文档文字包含不支持的控制字符，请重新导出文件".into(),
            ));
        }
        if target.len() + character.len_utf8() > input::MAX_FILE_BYTES {
            return Err(ServiceError::BadRequest(
                "文档提取文字（含来源位置和说明）超过 256 KiB，请拆分文档后添加".into(),
            ));
        }
        target.push(character);
    }
    Ok(())
}
