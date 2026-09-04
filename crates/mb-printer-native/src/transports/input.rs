use std::path::{Path, PathBuf};
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeInput {
    pub bytes: Vec<u8>,
    pub media_type: String,
    pub source: PathBuf,
}
pub trait NativeInputBackend {
    fn read(&self, path: &Path) -> Result<NativeInput, String>;
}
pub struct FileInputBackend {
    pub maximum_bytes: u64,
}
impl NativeInputBackend for FileInputBackend {
    fn read(&self, path: &Path) -> Result<NativeInput, String> {
        let metadata = std::fs::metadata(path).map_err(|error| error.to_string())?;
        if metadata.len() > self.maximum_bytes {
            return Err("native input exceeds configured limit".into());
        }
        let media_type = match path
            .extension()
            .and_then(|value| value.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("pdf") => "application/pdf",
            Some("png") => "image/png",
            Some("jpg" | "jpeg") => "image/jpeg",
            Some("svg") => "image/svg+xml",
            _ => return Err("unsupported native input type".into()),
        };
        Ok(NativeInput {
            bytes: std::fs::read(path).map_err(|error| error.to_string())?,
            media_type: media_type.into(),
            source: path.to_owned(),
        })
    }
}
