//! Files that look like keys, passwords or sign-ins, which a page should
//! get only when the user means it. Over ssh a picker browses a server,
//! whose deploy keys and environment files are worth more than a laptop's.
//! An agent's `upload` is refused them outright; the user is asked again.

use std::io::Read;
use std::path::Path;

/// Folders under home whose files are credentials, and what they hold.
const FOLDERS: [(&str, &str); 14] = [
    (".ssh", "your ssh keys"),
    (".gnupg", "your GPG keys"),
    (".aws", "your AWS credentials"),
    (".azure", "your Azure credentials"),
    (".kube", "your Kubernetes credentials"),
    (".docker", "your Docker credentials"),
    (".config/gcloud", "your Google Cloud credentials"),
    (".config/gh", "your GitHub sign-in"),
    (".password-store", "your passwords"),
    (".local/share/keyrings", "your keyrings"),
    ("Library/Keychains", "your keychains"),
    ("Library/Cookies", "your cookies"),
    (".kittybrowser", "kb's own profile, with your cookies and sign-ins"),
    (".mozilla", "a browser profile, with cookies and saved passwords"),
];

/// Files under home that hold passwords or tokens.
const FILES: [&str; 6] = [".netrc", ".npmrc", ".pypirc", ".git-credentials", ".pgpass", ".my.cnf"];

/// Why `path` looks secret, as words to finish "it holds ...", or None.
pub fn sensitive(path: &Path) -> Option<String> {
    let path = path.canonicalize().unwrap_or_else(|_| path.to_owned());
    if let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) {
        let home = home.canonicalize().unwrap_or(home);
        if let Ok(rest) = path.strip_prefix(&home) {
            for (folder, what) in FOLDERS {
                if rest.starts_with(folder) {
                    return Some(format!("it is in ~/{folder}, {what}"));
                }
            }
            if FILES.iter().any(|file| rest == Path::new(file)) {
                return Some("it holds passwords or tokens".to_owned());
            }
        }
    }
    // Browser profiles anywhere: cookies and saved passwords.
    let name = path.file_name().map_or(String::new(), |name| name.to_string_lossy().to_lowercase());
    let text = path.to_string_lossy();
    let in_profile = ["/Google/Chrome/", "/google-chrome/", "/Chromium/", "/chromium/", "/BraveSoftware/"]
        .iter()
        .any(|folder| text.contains(folder));
    if in_profile && ["cookies", "login data", "web data"].contains(&name.as_str()) {
        return Some("it holds a browser's cookies or saved passwords".to_owned());
    }
    if name == ".env" || name.starts_with(".env.") || name.ends_with(".env") {
        return Some("environment files often hold passwords and keys".to_owned());
    }
    if ["id_rsa", "id_dsa", "id_ecdsa", "id_ed25519"].iter().any(|key| name == *key) {
        return Some("it is a private ssh key".to_owned());
    }
    let extension = name.rsplit_once('.').map_or("", |(_, ext)| ext);
    if ["pem", "key", "p12", "pfx", "kdbx", "keychain", "keystore", "jks"].contains(&extension) {
        return Some(format!("a .{extension} file usually holds keys or passwords"));
    }
    if looks_like_private_key(&path) {
        return Some("it is a private key".to_owned());
    }
    None
}

/// Whether a file starts like a PEM or OpenSSH private key.
fn looks_like_private_key(path: &Path) -> bool {
    let Ok(file) = std::fs::File::open(path) else { return false };
    let mut start = Vec::new();
    if file.take(128).read_to_end(&mut start).is_err() {
        return false;
    }
    let start = String::from_utf8_lossy(&start);
    start.trim_start().starts_with("-----BEGIN") && start.contains("PRIVATE KEY")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_under_home_are_sensitive() {
        let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) else { return };
        assert!(sensitive(&home.join(".ssh/config")).unwrap().contains("ssh keys"));
        assert!(sensitive(&home.join(".aws/credentials")).is_some());
        assert!(sensitive(&home.join(".kittybrowser/profile/Cookies")).is_some());
        assert!(sensitive(&home.join(".netrc")).is_some());
        assert!(sensitive(&home.join("Documents/report.pdf")).is_none());
    }

    #[test]
    fn names_that_hold_secrets_are_sensitive_anywhere() {
        for name in ["/srv/app/.env", "/srv/app/.env.production", "/tmp/id_ed25519", "/tmp/server.pem", "/x/y.KEY"] {
            assert!(sensitive(Path::new(name)).is_some(), "{name}");
        }
        for name in ["/srv/app/env.md", "/tmp/id_ed25519.pub", "/tmp/photo.png", "/tmp/keynote.txt"] {
            assert!(sensitive(Path::new(name)).is_none(), "{name}");
        }
    }

    #[test]
    fn a_private_key_is_known_by_its_first_line() {
        let dir = std::env::temp_dir().join(format!("kb-secrets-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let key = dir.join("backup.txt");
        std::fs::write(&key, "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXk=\n").unwrap();
        let note = dir.join("note.txt");
        std::fs::write(&note, "-----BEGIN NOTE-----\n").unwrap();
        assert!(sensitive(&key).is_some());
        assert!(sensitive(&note).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
