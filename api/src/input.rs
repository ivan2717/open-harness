//! Validate explicit text snapshots independently of project filesystem permissions.
use crate::service::ServiceError;
use agent_core::TextFileReference;

const MAX_FILES: usize = 8;
const MAX_FILE_BYTES: usize = 256 * 1024;
const MAX_TOTAL_FILE_BYTES: usize = 512 * 1024;

pub(crate) fn validate(message: &str, files: &[TextFileReference]) -> Result<(), ServiceError> {
    if (message.trim().is_empty() && files.is_empty()) || message.chars().count() > 4000 {
        return Err(ServiceError::BadRequest(
            "输入最多 4000 个字符，请填写内容或添加文件引用".into(),
        ));
    }
    if files.len() > MAX_FILES {
        return Err(ServiceError::BadRequest("每条消息最多引用 8 个文件".into()));
    }
    let mut total = 0;
    for file in files {
        if file.name.trim().is_empty()
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
                "暂不支持此文件的内容，请添加 UTF-8 文本或代码文件".into(),
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
