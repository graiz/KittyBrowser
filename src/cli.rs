//! `kb ctl`: give one command to the running kb and print its answer.

use crate::commands::{self, COMMANDS};
use crate::control::{self, Output};
use crate::term::printable;
use serde_json::json;
use std::path::{Path, PathBuf};

fn usage() -> String {
    let mut text = String::from(
        "usage: kb ctl <command> [arguments]\n\n\
         Controls the kb browser running in a terminal pane: what you do here\n\
         happens on the page the user is looking at. With several kbs running,\n\
         kb ctl -i ID <command> picks one, by the number, id or name kb list\n\
         shows. Without it: the kb in your Rex session, else the\n\
         lowest-numbered.\n\n",
    );
    for command in COMMANDS {
        text.push_str(&format!("  {}\n      {}\n", command.synopsis(), command.help));
    }
    text.push_str(
        "\nA <selector> is a CSS selector (the first match is used; it also finds\n\
         elements inside shadow DOM, and a >>> b picks b inside a's shadow root,\n\
         or inside the page in iframe a, if it is from the same site),\n\
         or text=Some words for the element showing that text, or @N for link N\n\
         from kb ctl links, or :focus for whatever has focus.\n\
         kb ctl <command> --help describes one command and its arguments.\n\
         Exit status: 0 on success; 1 if the command failed, with the reason on\n\
         stderr; 2 if the command line was wrong. A command that worked but\n\
         found errors on the page (open, reload, click) still exits 0 and says\n\
         so in its output; in a script, follow it with kb ctl check.",
    );
    text
}

/// Whether an address that isn't an existing file was still meant as one,
/// rather than as a host name or a search.
fn looks_like_a_file(address: &str) -> bool {
    let lower = address.to_ascii_lowercase();
    if address.contains("://") {
        return false;
    }
    if address.starts_with(['.', '/', '~']) {
        return true;
    }
    // "site.com/page.html" and "localhost:8000/page.html" are a host and a
    // path; "pages/about.html" and "about.html" are files.
    let host_like = match address.split_once('/') {
        Some((first, _)) => first.contains(['.', ':']) || first == "localhost",
        None => false,
    };
    !host_like && [".html", ".htm", ".xhtml", ".svg", ".pdf"].iter().any(|ending| lower.ends_with(ending))
}

/// Run `kb ctl` with the words after it. Returns the exit status.
pub fn run(mut words: &[String]) -> i32 {
    // Which kb, when several are running.
    if let [flag, which, rest @ ..] = words {
        if flag == "-i" || flag == "--instance" {
            std::env::set_var("KB_INSTANCE", which);
            words = rest;
        }
    }
    let Some(name) = words.first().filter(|word| !matches!(word.as_str(), "-h" | "--help" | "help")) else {
        println!("{}", usage());
        return 0;
    };
    let Some(command) = commands::find(name) else {
        let names: Vec<&str> = COMMANDS.iter().map(|command| command.name).collect();
        eprintln!("kb ctl: unknown command {name:?}\ncommands: {}\nsee: kb ctl --help", names.join(", "));
        return 2;
    };

    let words = &words[1..];
    // Anywhere, as an agent adds it to a command it was about to run.
    if words.iter().any(|word| word == "--help" || word == "-h") {
        println!("{}", command.describe());
        return 0;
    }
    let mut args = match command.parse(words) {
        Ok(args) => args,
        Err(error) => {
            eprintln!("kb ctl: {}", printable(&error));
            return 2;
        }
    };
    if let Err(error) = command.resolve_files(&mut args) {
        eprintln!("kb ctl: {}", printable(&error));
        return 1;
    }
    // Where to save an image is ours to deal with, not the browser's.
    let out = args.as_object_mut().and_then(|args| args.remove(commands::OUT));
    let out = out.as_ref().and_then(|out| out.as_str()).map(PathBuf::from);
    // kb's working directory isn't ours, so a relative file path has to be
    // made absolute here.
    if let Some(url) = args["url"].as_str() {
        match std::fs::canonicalize(url) {
            Ok(path) => args["url"] = json!(format!("file://{}", path.display())),
            Err(_) if looks_like_a_file(url) => {
                let path = std::env::current_dir().unwrap_or_default().join(url);
                eprintln!("kb ctl: no such file: {}", path.display());
                return 1;
            }
            Err(_) => {}
        }
    }

    match control::request(command.name, args) {
        Ok(Output::Text(text)) => {
            println!("{}", printable(&text));
            0
        }
        Ok(Output::Image(png)) => {
            let saved = crate::base64::decode(&png)
                .ok_or("kb sent a damaged image".to_owned())
                .and_then(|bytes| save_image(out.as_deref(), &bytes));
            match saved {
                Ok(path) => {
                    if let Some(path) = path {
                        println!("{}", printable(&path.display().to_string()));
                    }
                    0
                }
                Err(error) => {
                    eprintln!("kb ctl: {}", printable(&error));
                    1
                }
            }
        }
        Err(error) => {
            eprintln!("kb ctl: {}", printable(&error));
            1
        }
    }
}

/// Save a screenshot at `out`, or without it in a folder of the user's own
/// in the temporary directory, which the system clears of old files, and
/// return where, if it is a file. Only the user can read one saved there:
/// it can show whatever a signed-in page does.
fn save_image(out: Option<&Path>, bytes: &[u8]) -> Result<Option<PathBuf>, String> {
    match out {
        Some(out) => save_out(out, bytes),
        None => {
            let folder = screenshot_folder(&std::env::temp_dir(), unsafe { libc::geteuid() })?;
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_millis());
            save_new(&folder, stamp, bytes).map(Some)
        }
    }
}

/// The folder in `temp` that screenshots go in: kb-screenshots-UID, named
/// for the user as /tmp is shared on Linux. Should another user have made
/// it first, a new one with a name no one can guess.
fn screenshot_folder(temp: &Path, uid: u32) -> Result<PathBuf, String> {
    let folder = temp.join(format!("kb-screenshots-{uid}"));
    match crate::files::private_shared_dir(&folder) {
        Ok(()) => Ok(folder),
        // A new one each time while the name is taken; the system clears
        // them with the rest of the temporary directory.
        Err(_) => crate::files::private_temp_dir(&format!("kb-screenshots-{uid}-"))
            .map_err(|e| format!("making a folder for the screenshot: {e}")),
    }
}

/// Save `bytes` in a new file in `folder`, named for `stamp`, and a number
/// after it should two screenshots come in the same millisecond.
fn save_new(folder: &Path, stamp: u128, bytes: &[u8]) -> Result<PathBuf, String> {
    for n in 0..100 {
        let name = match n {
            0 => format!("kb-screenshot-{stamp}.png"),
            n => format!("kb-screenshot-{stamp}-{n}.png"),
        };
        let path = folder.join(name);
        match crate::files::write_new(&path, bytes, 0o600) {
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            result => return result.map(|_| path.clone()).map_err(|e| format!("writing {}: {e}", path.display())),
        }
    }
    Err(format!("couldn't find a free name for a screenshot in {}", folder.display()))
}

/// Save `bytes` at `path` as cp or curl -o would: a file there is
/// replaced, a link followed, and a pipe or a device written into. Returns
/// the path to print if the image went to a file, and not to a pipe, a
/// device or kb ctl's own output (--out /dev/stdout > shot.png), where the
/// path would land in the image.
fn save_out(path: &Path, bytes: &[u8]) -> Result<Option<PathBuf>, String> {
    use std::io::Write;
    use std::os::fd::AsFd;
    use std::os::unix::fs::MetadataExt;
    if path.is_dir() {
        return Err(format!("{} is a folder; give a file name", path.display()));
    }
    if let Some(folder) = path.parent().filter(|folder| !folder.as_os_str().is_empty()) {
        let _ = std::fs::create_dir_all(folder);
    }
    let fail = |e: std::io::Error| format!("writing {}: {e}", path.display());
    let mut file = std::fs::File::create(path).map_err(fail)?;
    file.write_all(bytes).map_err(fail)?;
    let written = file.metadata().map_err(fail)?;
    let output =
        std::io::stdout().as_fd().try_clone_to_owned().ok().and_then(|out| std::fs::File::from(out).metadata().ok());
    let ours = output.is_some_and(|out| (out.dev(), out.ino()) == (written.dev(), written.ino()));
    Ok((written.is_file() && !ours).then(|| path.to_owned()))
}

/// `kb list`: the kbs running now, one per line, by number or name.
/// Returns the exit status.
pub fn list() -> i32 {
    // Otherwise a data directory kb refuses would read as no kb running.
    if let Err(error) = crate::data_dir() {
        eprintln!("kb: {}", printable(&error));
        return 1;
    }
    let running = control::running();
    if running.is_empty() {
        eprintln!("kb: {}", control::NOT_RUNNING);
        return 1;
    }
    for instance in running {
        std::env::set_var("KB_INSTANCE", &instance);
        let (page, pane, id) = match control::request("status", json!({"listing": true})) {
            Ok(Output::Text(status)) => {
                let field =
                    |name: &str| status.lines().find_map(|line| line.strip_prefix(name)).unwrap_or("").to_owned();
                let url = field("url: ");
                (if url.is_empty() { "about:blank".to_owned() } else { url }, field("pane: "), field("id: "))
            }
            _ => ("(not answering)".to_owned(), String::new(), String::new()),
        };
        // The pane tells kbs apart: an agent knows the one it split. The id
        // names this run of it, so a number taken over later isn't mistaken.
        println!("{}", printable(&format!("{instance}\t{page}\t{pane}\t{id}")));
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let folder = std::env::temp_dir().join(format!("kb-cli-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir(&folder).unwrap();
        folder
    }

    fn mode(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::symlink_metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn screenshots_are_private_and_new() {
        let folder = scratch("new");
        let first = save_new(&folder, 1700000000000, b"one").unwrap();
        assert_eq!(first, folder.join("kb-screenshot-1700000000000.png"));
        assert_eq!(mode(&first), 0o600);
        // The same millisecond again: another file, not the first replaced.
        let second = save_new(&folder, 1700000000000, b"two").unwrap();
        assert_eq!(second, folder.join("kb-screenshot-1700000000000-1.png"));
        assert_eq!(std::fs::read(&first).unwrap(), b"one");
        // A link planted where the next would go isn't written through.
        let target = folder.join("target");
        std::fs::write(&target, "untouched").unwrap();
        std::os::unix::fs::symlink(&target, folder.join("kb-screenshot-5.png")).unwrap();
        assert_eq!(save_new(&folder, 5, b"three").unwrap(), folder.join("kb-screenshot-5-1.png"));
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "untouched");
        std::fs::remove_dir_all(&folder).unwrap();
    }

    #[test]
    fn out_writes_as_cp_would() {
        let folder = scratch("out");
        // A link is followed, as cp follows one.
        let target = folder.join("target");
        std::fs::write(&target, "old").unwrap();
        let out = folder.join("shot.png");
        std::os::unix::fs::symlink(&target, &out).unwrap();
        assert_eq!(save_image(Some(&out), b"frame").unwrap(), Some(out.clone()));
        assert_eq!(std::fs::read(&target).unwrap(), b"frame");
        assert!(std::fs::symlink_metadata(&out).unwrap().file_type().is_symlink());
        // A new file is made as any of the user's is, not private: it may
        // be meant for a server or a container to read.
        let new = folder.join("new.png");
        save_image(Some(&new), b"frame").unwrap();
        std::fs::write(folder.join("plain"), "").unwrap();
        assert_eq!(mode(&new), mode(&folder.join("plain")));
        // A file is replaced, into a folder made for it.
        let nested = folder.join("a/b/shot.png");
        save_image(Some(&nested), b"first, longer").unwrap();
        save_image(Some(&nested), b"second").unwrap();
        assert_eq!(std::fs::read(&nested).unwrap(), b"second");
        assert_eq!(std::fs::read_dir(folder.join("a/b")).unwrap().count(), 1);
        // A folder in the way is an error.
        let error = save_image(Some(&folder.join("a")), b"x").unwrap_err();
        assert!(error.contains("is a folder"), "{error}");
        std::fs::remove_dir_all(&folder).unwrap();
    }

    #[test]
    fn out_keeps_the_mode_of_the_file_it_replaces() {
        use std::os::unix::fs::PermissionsExt;
        let folder = scratch("mode");
        let out = folder.join("shot.png");
        std::fs::write(&out, "old").unwrap();
        std::fs::set_permissions(&out, std::fs::Permissions::from_mode(0o640)).unwrap();
        save_image(Some(&out), b"new").unwrap();
        assert_eq!(std::fs::read(&out).unwrap(), b"new");
        assert_eq!(mode(&out), 0o640);
        std::fs::remove_dir_all(&folder).unwrap();
    }

    #[test]
    fn out_writes_into_a_pipe_or_a_link_to_one() {
        let folder = scratch("pipe");
        let pipe = folder.join("pipe");
        let name = std::ffi::CString::new(pipe.as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        // As /dev/stdout is a link to the process's output.
        let link = folder.join("stdout");
        std::os::unix::fs::symlink(&pipe, &link).unwrap();
        for out in [&pipe, &link] {
            let reader = {
                let pipe = pipe.clone();
                std::thread::spawn(move || std::fs::read(pipe).unwrap())
            };
            // No path to print: the pipe has the image.
            assert_eq!(save_image(Some(out), b"png bytes").unwrap(), None);
            assert_eq!(reader.join().unwrap(), b"png bytes");
        }
        assert!(std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
        assert_eq!(std::fs::read_dir(&folder).unwrap().count(), 2);
        std::fs::remove_dir_all(&folder).unwrap();
    }

    #[test]
    fn screenshots_go_elsewhere_if_their_folder_is_taken() {
        let temp = scratch("taken");
        assert_eq!(screenshot_folder(&temp, 4242).unwrap(), temp.join("kb-screenshots-4242"));
        // Another user's, or a link in its place, as one could plant in /tmp.
        std::fs::remove_dir(temp.join("kb-screenshots-4242")).unwrap();
        std::os::unix::fs::symlink(&temp, temp.join("kb-screenshots-4242")).unwrap();
        let instead = screenshot_folder(&temp, 4242).unwrap();
        let name = instead.file_name().unwrap().to_str().unwrap();
        assert!(name.starts_with("kb-screenshots-4242-") && name.len() == "kb-screenshots-4242-".len() + 6);
        assert_eq!(mode(&instead), 0o700);
        std::fs::remove_dir(&instead).unwrap();
        std::fs::remove_dir_all(&temp).unwrap();
    }

    #[test]
    fn files_and_hosts() {
        assert!(looks_like_a_file("nothere.html"));
        assert!(looks_like_a_file("./site"));
        assert!(looks_like_a_file("/tmp/x"));
        assert!(looks_like_a_file("pages/about.html"));
        assert!(!looks_like_a_file("127.0.0.1:49844/index.html"));
        assert!(!looks_like_a_file("example.com/index.html"));
        assert!(!looks_like_a_file("example.com"));
        assert!(!looks_like_a_file("localhost:3000"));
        assert!(!looks_like_a_file("https://example.com/index.html"));
    }
}
