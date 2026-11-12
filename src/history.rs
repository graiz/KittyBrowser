//! Pages visited, kept on disk to complete addresses as they are typed.
//!
//! The file holds one page per line: visit count, time of the last visit in
//! seconds since the epoch, and the URL, separated by tabs.

use std::fmt::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Pages remembered. Past this, the ones visited longest ago are dropped.
const MAX_ENTRIES: usize = 5000;
/// Changes are written out at most this often, and on exit.
const SAVE_INTERVAL: Duration = Duration::from_secs(2);

#[derive(Clone, PartialEq, Eq, Debug)]
struct Entry {
    url: String,
    visits: u32,
    last: u64,
}

pub struct History {
    path: PathBuf,
    entries: Vec<Entry>,
    dirty: bool,
    saved: Instant,
}

impl History {
    /// Read the history at `path`; a missing or unreadable file is an empty one.
    pub fn load(path: PathBuf) -> History {
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let entries = text
            .lines()
            .filter_map(|line| {
                let mut fields = line.splitn(3, '\t');
                let visits = fields.next()?.parse().ok()?;
                let last = fields.next()?.parse().ok()?;
                Some(Entry { url: fields.next()?.to_owned(), visits, last })
            })
            .collect();
        History { path, entries, dirty: false, saved: Instant::now() }
    }

    /// Count a visit to `url`. Only web pages are remembered.
    pub fn record(&mut self, url: &str) {
        if !(url.starts_with("https://") || url.starts_with("http://")) {
            return;
        }
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
        match self.entries.iter_mut().find(|entry| entry.url == url) {
            Some(entry) => {
                entry.visits += 1;
                entry.last = now;
            }
            None => self.entries.push(Entry { url: url.to_owned(), visits: 1, last: now }),
        }
        self.dirty = true;
    }

    /// Write changes out, if there are any and the last write wasn't just now.
    pub fn save_if_due(&mut self) {
        if self.saved.elapsed() >= SAVE_INTERVAL {
            self.save();
        }
    }

    pub fn save(&mut self) {
        if !self.dirty {
            return;
        }
        if self.entries.len() > MAX_ENTRIES {
            self.entries.sort_by_key(|entry| std::cmp::Reverse(entry.last));
            self.entries.truncate(MAX_ENTRIES);
        }
        let mut text = String::new();
        for entry in &self.entries {
            let _ = writeln!(text, "{}\t{}\t{}", entry.visits, entry.last, entry.url);
        }
        // Losing history is not worth interrupting browsing over.
        let _ = std::fs::write(&self.path, text);
        self.dirty = false;
        self.saved = Instant::now();
    }

    /// What to append to `typed` to make it an address visited before, taken
    /// from the most visited page that starts with it. The scheme and a
    /// leading "www." needn't be typed.
    ///
    /// While still typing the host, only the host is completed, so that Enter
    /// goes to the site rather than to some page deep inside it.
    pub fn complete(&self, typed: &str) -> Option<String> {
        if typed.is_empty() || typed.contains(char::is_whitespace) {
            return None;
        }
        let mut best: Option<(&Entry, &str)> = None;
        for entry in &self.entries {
            let better = best.is_none_or(|(b, _)| (entry.visits, entry.last) > (b.visits, b.last));
            if !better {
                continue;
            }
            let rest = entry.url.split_once("://").map_or(entry.url.as_str(), |(_, rest)| rest);
            let forms = [rest.strip_prefix("www.").unwrap_or(rest), rest, entry.url.as_str()];
            let suffix = forms.into_iter().find_map(|form| completion(form, typed));
            if let Some(suffix) = suffix {
                best = Some((entry, suffix));
            }
        }
        best.map(|(_, suffix)| suffix.to_owned())
    }
}

/// The part of `address` that completes `typed`, if it starts with `typed`
/// and there is something worth adding.
fn completion<'a>(address: &'a str, typed: &str) -> Option<&'a str> {
    let prefix = address.as_bytes().get(..typed.len())?;
    if !prefix.eq_ignore_ascii_case(typed.as_bytes()) {
        return None;
    }
    let host_start = address.find("://").map_or(0, |at| at + 3);
    let host_end = address[host_start..].find('/').map_or(address.len(), |at| host_start + at);
    let suffix = if typed.len() <= host_end {
        &address[typed.len()..host_end]
    } else {
        // "site.com/" alone asks for the site's front page, not a guess.
        address.get(typed.len()..).filter(|_| typed.len() > host_end + 1)?
    };
    (!suffix.is_empty()).then_some(suffix)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn history(pages: &[(&str, u32)]) -> History {
        let entries = pages
            .iter()
            .enumerate()
            .map(|(i, (url, visits))| Entry { url: (*url).to_owned(), visits: *visits, last: i as u64 })
            .collect();
        History { path: PathBuf::new(), entries, dirty: false, saved: Instant::now() }
    }

    #[test]
    fn completes_the_host() {
        let h = history(&[("https://gregraiz.com/blog/a-post", 5), ("https://www.rust-lang.org/", 1)]);
        assert_eq!(h.complete("gre").as_deref(), Some("graiz.com"));
        assert_eq!(h.complete("GRE").as_deref(), Some("graiz.com"));
        assert_eq!(h.complete("rust").as_deref(), Some("-lang.org"));
        assert_eq!(h.complete("www.ru").as_deref(), Some("st-lang.org"));
        assert_eq!(h.complete("https://gr").as_deref(), Some("egraiz.com"));
        assert_eq!(h.complete("nothing"), None);
        assert_eq!(h.complete(""), None);
        assert_eq!(h.complete("gre graiz"), None);
    }

    #[test]
    fn a_whole_host_is_left_alone() {
        let h = history(&[("https://gregraiz.com/blog/a-post", 5)]);
        assert_eq!(h.complete("gregraiz.com"), None);
        assert_eq!(h.complete("gregraiz.com/"), None);
    }

    #[test]
    fn completes_a_path_once_one_is_typed() {
        let h = history(&[("https://gregraiz.com/blog/a-post", 5)]);
        assert_eq!(h.complete("gregraiz.com/b").as_deref(), Some("log/a-post"));
        assert_eq!(h.complete("https://gregraiz.com/blog/").as_deref(), Some("a-post"));
    }

    #[test]
    fn prefers_the_most_visited_then_the_latest() {
        let h = history(&[("https://example.com/", 2), ("https://exotic.org/", 9), ("https://extra.net/", 9)]);
        assert_eq!(h.complete("ex").as_deref(), Some("tra.net"));
        assert_eq!(h.complete("exa").as_deref(), Some("mple.com"));
    }

    #[test]
    fn records_and_reloads() {
        let path = std::env::temp_dir().join(format!("kb-history-test-{}", std::process::id()));
        let mut h = History::load(path.clone());
        h.record("https://a.example/");
        h.record("https://a.example/");
        h.record("https://b.example/x y");
        h.record("about:blank");
        h.record("file:///etc/hosts");
        h.save();
        let reloaded = History::load(path.clone());
        let _ = std::fs::remove_file(&path);
        let seen: Vec<(&str, u32)> = reloaded.entries.iter().map(|e| (e.url.as_str(), e.visits)).collect();
        assert_eq!(seen, [("https://a.example/", 2), ("https://b.example/x y", 1)]);
    }
}
