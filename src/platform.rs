//! Operating-system services shared by the browser and its tools.

use std::path::{Path, PathBuf};

#[cfg(windows)]
pub mod windows;

pub fn home() -> Option<PathBuf> {
    #[cfg(unix)]
    {
        std::env::var_os("HOME").map(PathBuf::from)
    }
    #[cfg(windows)]
    {
        windows::known_folder(&windows_sys::Win32::UI::Shell::FOLDERID_Profile).ok()
    }
}

pub fn file_url(path: &Path) -> Result<String, String> {
    let path = path.canonicalize().map_err(|e| format!("opening {}: {e}", path.display()))?;
    let text = path.to_str().ok_or("the file path isn't valid Unicode")?;
    #[cfg(windows)]
    let text = text
        .strip_prefix(r"\\?\UNC\")
        .map(|rest| format!("//{rest}"))
        .unwrap_or_else(|| text.strip_prefix(r"\\?\").unwrap_or(text).to_owned())
        .replace('\\', "/");
    let mut encoded = String::new();
    for byte in text.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' | b':' => {
                encoded.push(*byte as char)
            }
            _ => {
                use std::fmt::Write;
                let _ = write!(encoded, "%{byte:02X}");
            }
        }
    }
    #[cfg(windows)]
    if let Some(unc) = encoded.strip_prefix("//") {
        return Ok(format!("file://{unc}"));
    }
    Ok(format!("file://{}{}", if encoded.starts_with('/') { "" } else { "/" }, encoded))
}

#[cfg(test)]
mod tests {
    #[test]
    fn local_file_urls_escape_names() {
        let folder = crate::files::private_temp_dir("kb-url-test-").unwrap();
        let path = folder.join("a #%.html");
        std::fs::write(&path, b"").unwrap();
        let url = super::file_url(&path).unwrap();
        assert!(url.starts_with("file:///"), "{url}");
        assert!(url.ends_with("a%20%23%25.html"), "{url}");
        assert!(!url.contains('\\'));
        std::fs::remove_dir_all(folder).unwrap();
    }
}
