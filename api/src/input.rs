//! Validate explicit attachment snapshots independently of project filesystem permissions.
use crate::service::ServiceError;
use agent_core::TextFileReference;

const MAX_FILES: usize = 64;
pub(crate) const MAX_FILE_BYTES: usize = 256 * 1024;
const MAX_TOTAL_FILE_BYTES: usize = 512 * 1024;

pub(crate) fn validate(message: &str, files: &[TextFileReference]) -> Result<(), ServiceError> {
    if (message.trim().is_empty() && files.is_empty()) || message.chars().count() > 4000 {
        return Err(ServiceError::BadRequest(
            "输入最多 4000 个字符，请填写内容或添加文件引用".into(),
        ));
    }
    if files.len() > MAX_FILES {
        return Err(ServiceError::BadRequest(
            "每条消息最多引用 64 个文件".into(),
        ));
    }
    if files.iter().filter(|file| file.image.is_some()).count() > agent_core::MAX_TOOL_IMAGES {
        return Err(ServiceError::BadRequest("每条消息最多添加 4 张图片".into()));
    }
    let mut total = 0;
    for file in files {
        if file.name.trim().is_empty()
            || matches!(file.name.as_str(), "." | "..")
            || file.name.chars().count() > 255
            || file.name.chars().any(char::is_control)
            || file.name.contains(['/', '\\'])
        {
            return Err(ServiceError::BadRequest("文件名无效".into()));
        }
        if file.path.as_ref().is_some_and(|path| {
            path.trim().is_empty()
                || path.chars().count() > 4096
                || path.chars().any(char::is_control)
        }) {
            return Err(ServiceError::BadRequest("文件显示路径无效".into()));
        }
        if let Some(attachment) = &file.attachment {
            if file.image.is_some() || !file.text.is_empty() {
                return Err(ServiceError::BadRequest(
                    "原文件附件不能同时包含文字快照或图片".into(),
                ));
            }
            if !super::service::attachments::valid_attachment_id(&attachment.id)
                || attachment.size > super::service::attachments::MAX_ATTACHMENT_BYTES as u64
            {
                return Err(ServiceError::BadRequest("原文件附件标识或大小无效".into()));
            }
        }
        if let Some(image) = &file.image {
            if !file.text.is_empty() {
                return Err(ServiceError::BadRequest(
                    "附件不能同时包含文字快照和图片".into(),
                ));
            }
            image.validate().map_err(|_| {
                ServiceError::BadRequest(
                    "图片无效：支持 PNG、JPEG、GIF、WebP，单张最多 5 MiB、约 4 百万像素".into(),
                )
            })?;
        }
        if file.text.len() > MAX_FILE_BYTES {
            return Err(ServiceError::BadRequest(
                "单个引用文件不能超过 256 KiB".into(),
            ));
        }
        if file
            .text
            .chars()
            .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
        {
            return Err(ServiceError::BadRequest(
                "引用文字包含不支持的控制字符，请重新导出文件".into(),
            ));
        }
        total += file.text.len();
    }
    if total > MAX_TOTAL_FILE_BYTES {
        return Err(ServiceError::BadRequest(
            "每条消息的引用文件总量不能超过 512 KiB".into(),
        ));
    }
    Ok(())
}

pub(crate) fn title(message: &str, files: &[TextFileReference]) -> String {
    if message.trim().is_empty() {
        files
            .iter()
            .map(|file| file.name.as_str())
            .collect::<Vec<_>>()
            .join("、")
            .chars()
            .take(64)
            .collect()
    } else {
        message.trim().chars().take(64).collect()
    }
}

/// Explicitly reject new raster input when the configured model has no vision.
pub(crate) fn validate_images(
    files: &[TextFileReference],
    supports_images: bool,
) -> Result<(), ServiceError> {
    if !supports_images && files.iter().any(|file| file.image.is_some()) {
        return Err(ServiceError::BadRequest(
            "当前模型未启用图片输入，请选择支持图片的模型，或在模型配置中开启图片输入".into(),
        ));
    }
    Ok(())
}
