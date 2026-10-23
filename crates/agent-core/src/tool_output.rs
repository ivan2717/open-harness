//! Image-bearing tool observations. Image bytes never masquerade as model text.

use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const MAX_IMAGE_BYTES: usize = 5 * 1024 * 1024;
pub const MAX_IMAGE_PIXELS: u64 = 4_194_304;
pub const MAX_TOOL_IMAGES: usize = 4;
/// Conservative reservation for one screenshot. This is a budget allowance,
/// not a tokenizer count; actual provider usage calibrates subsequent requests.
pub const IMAGE_TOKEN_ALLOWANCE: usize = 8_192;

#[derive(Clone, Eq, Serialize, Deserialize)]
pub struct ModelImage {
    pub mime_type: String,
    /// Inline base64 while active; `sha256:<id>` in archived runtime history.
    /// Only EventStore may resolve references. Providers reject unresolved data.
    pub data_base64: String,
}

impl PartialEq for ModelImage {
    fn eq(&self, other: &Self) -> bool {
        self.mime_type == other.mime_type
            && (self.data_base64 == other.data_base64
                || self
                    .content_id()
                    .ok()
                    .zip(other.content_id().ok())
                    .is_some_and(|(a, b)| a == b))
    }
}

impl std::fmt::Debug for ModelImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModelImage")
            .field("mime_type", &self.mime_type)
            .field("encoded_bytes", &self.data_base64.len())
            .finish()
    }
}

impl ModelImage {
    pub fn reference_id(&self) -> Option<&str> {
        let id = self.data_base64.strip_prefix("sha256:")?;
        (id.len() == 64
            && id
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)))
        .then_some(id)
    }

    pub fn content_id(&self) -> Result<String, ImageError> {
        if let Some(id) = self.reference_id() {
            return Ok(id.to_owned());
        }
        Ok(format!("{:x}", Sha256::digest(self.decoded_bytes()?)))
    }

    /// Retain identity without retaining raster data. The caller must have
    /// durably stored the image before releasing its last inline copy.
    pub fn as_reference(&self) -> Result<Self, ImageError> {
        Ok(Self {
            mime_type: self.mime_type.clone(),
            data_base64: format!("sha256:{}", self.content_id()?),
        })
    }
    pub fn from_bytes(mime_type: impl Into<String>, bytes: &[u8]) -> Result<Self, ImageError> {
        if bytes.len() > MAX_IMAGE_BYTES {
            return Err(ImageError("image exceeds the 5 MiB limit"));
        }
        Self::new(mime_type, STANDARD.encode(bytes))
    }
    pub fn new(
        mime_type: impl Into<String>,
        data_base64: impl Into<String>,
    ) -> Result<Self, ImageError> {
        let image = Self {
            mime_type: mime_type.into(),
            data_base64: data_base64.into(),
        };
        image.validate()?;
        Ok(image)
    }

    pub fn validate(&self) -> Result<(), ImageError> {
        self.decoded_bytes().map(|_| ())
    }

    pub fn decoded_bytes(&self) -> Result<Vec<u8>, ImageError> {
        if self.data_base64.len() > MAX_IMAGE_BYTES.div_ceil(3) * 4 {
            return Err(ImageError("image exceeds the 5 MiB limit"));
        }
        let bytes = STANDARD
            .decode(&self.data_base64)
            .map_err(|_| ImageError("invalid image base64"))?;
        if bytes.len() > MAX_IMAGE_BYTES {
            return Err(ImageError("image exceeds the 5 MiB limit"));
        }
        let (width, height) = image_dimensions(&self.mime_type, &bytes).ok_or(ImageError(
            "unsupported image type or invalid image dimensions/header",
        ))?;
        if width == 0
            || height == 0
            || width > 8192
            || height > 8192
            || u64::from(width) * u64::from(height) > MAX_IMAGE_PIXELS
        {
            return Err(ImageError(
                "image exceeds the 4 megapixel or 8192-pixel edge limit; capture a smaller viewport or resize before sending",
            ));
        }
        Ok(bytes)
    }

    pub fn data_url(&self) -> String {
        format!("data:{};base64,{}", self.mime_type, self.data_base64)
    }
}

// Read bounded headers only. Pixel data is never decompressed in the harness;
// byte and pixel limits apply before a provider or UI decoder sees the image.
fn image_dimensions(mime: &str, bytes: &[u8]) -> Option<(u32, u32)> {
    match mime {
        "image/png"
            if bytes.starts_with(b"\x89PNG\r\n\x1a\n")
                && bytes.get(8..16)? == b"\0\0\0\rIHDR"
                && bytes.len() >= 33 =>
        {
            Some((
                u32::from_be_bytes(bytes.get(16..20)?.try_into().ok()?),
                u32::from_be_bytes(bytes.get(20..24)?.try_into().ok()?),
            ))
        }
        "image/jpeg" if bytes.starts_with(&[0xff, 0xd8]) && bytes.ends_with(&[0xff, 0xd9]) => {
            jpeg_dimensions(bytes)
        }
        "image/gif"
            if (bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a"))
                && bytes.len() >= 13 =>
        {
            Some((
                u16::from_le_bytes(bytes.get(6..8)?.try_into().ok()?).into(),
                u16::from_le_bytes(bytes.get(8..10)?.try_into().ok()?).into(),
            ))
        }
        "image/webp" if bytes.starts_with(b"RIFF") && bytes.get(8..12)? == b"WEBP" => {
            webp_dimensions(bytes)
        }
        _ => None,
    }
}

fn jpeg_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    let mut offset = 2;
    while offset < bytes.len() {
        if *bytes.get(offset)? != 0xff {
            return None;
        }
        while *bytes.get(offset)? == 0xff {
            offset += 1;
        }
        let marker = *bytes.get(offset)?;
        offset += 1;
        if matches!(marker, 0xda | 0xd9 | 0x00) {
            return None;
        }
        if matches!(marker, 0x01 | 0xd0..=0xd8) {
            continue;
        }
        let length = u16::from_be_bytes(bytes.get(offset..offset + 2)?.try_into().ok()?) as usize;
        if length < 2 || offset.checked_add(length)? > bytes.len() {
            return None;
        }
        if matches!(marker, 0xc0..=0xc3 | 0xc5..=0xc7 | 0xc9..=0xcb | 0xcd..=0xcf) {
            if length < 8 {
                return None;
            }
            return Some((
                u16::from_be_bytes(bytes.get(offset + 5..offset + 7)?.try_into().ok()?).into(),
                u16::from_be_bytes(bytes.get(offset + 3..offset + 5)?.try_into().ok()?).into(),
            ));
        }
        offset += length;
    }
    None
}

fn webp_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if u32::from_le_bytes(bytes.get(4..8)?.try_into().ok()?) as usize + 8 != bytes.len() {
        return None;
    }
    match bytes.get(12..16)? {
        b"VP8X" if bytes.len() >= 30 && bytes[20] & 0x02 == 0 => Some((
            1 + u32::from_le_bytes([bytes[24], bytes[25], bytes[26], 0]),
            1 + u32::from_le_bytes([bytes[27], bytes[28], bytes[29], 0]),
        )),
        b"VP8L" if bytes.len() >= 25 && bytes[20] == 0x2f => Some((
            1 + u32::from(bytes[21]) + (u32::from(bytes[22] & 0x3f) << 8),
            1 + u32::from(bytes[22] >> 6)
                + (u32::from(bytes[23]) << 2)
                + (u32::from(bytes[24] & 0x0f) << 10),
        )),
        b"VP8 " if bytes.get(23..26)? == b"\x9d\x01\x2a" => Some((
            u32::from(u16::from_le_bytes(bytes.get(26..28)?.try_into().ok()?) & 0x3fff),
            u32::from(u16::from_le_bytes(bytes.get(28..30)?.try_into().ok()?) & 0x3fff),
        )),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ToolOutput {
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<ModelImage>,
}

impl ToolOutput {
    pub fn validate(&self) -> Result<(), ImageError> {
        if self.images.len() > MAX_TOOL_IMAGES {
            return Err(ImageError("tool output exceeds the four-image limit"));
        }
        self.images.iter().try_for_each(ModelImage::validate)
    }
}

impl From<String> for ToolOutput {
    fn from(text: String) -> Self {
        Self {
            text,
            images: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageError(pub &'static str);
impl std::fmt::Display for ImageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for ImageError {}
