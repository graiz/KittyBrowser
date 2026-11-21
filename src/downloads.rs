//! Files the pages download: what Chrome reports about each, and how kb
//! says it. Chrome saves them where it would for the user, so a repeated
//! name gets a number, as in any browser; kb only listens.

use serde_json::Value;

/// Finished downloads kept, to answer the command that caused them.
const MAX_FINISHED: usize = 20;

struct Download {
    guid: String,
    /// The name Chrome chose, from the link, the server or the address.
    name: String,
    received: u64,
    /// The size, when the server said it.
    total: Option<u64>,
    state: Saving,
    /// Position in the stream of page messages when it began, to tell
    /// which command set it off.
    seq: u64,
}

#[derive(Clone, Debug, PartialEq)]
enum Saving {
    Running,
    /// Saved, at this path.
    Saved(String),
    Failed,
}

impl Download {
    fn line(&self) -> String {
        match &self.state {
            Saving::Running => format!("downloading {}, {} so far", self.name, progress(self.received, self.total)),
            Saving::Saved(path) => format!("downloaded {} ({}) to {path}", self.name, size(self.received)),
            Saving::Failed => format!("the download of {} failed or was cancelled", self.name),
        }
    }

    fn running(&self) -> bool {
        self.state == Saving::Running
    }
}

/// Downloads under way, and the last few finished.
#[derive(Default)]
pub struct Downloads {
    list: Vec<Download>,
}

impl Downloads {
    /// Chrome's `Browser.downloadWillBegin`; `seq` places it among the
    /// page's messages.
    pub fn began(&mut self, params: &Value, seq: u64) {
        if self.list.iter().filter(|d| !d.running()).count() >= MAX_FINISHED {
            if let Some(oldest) = self.list.iter().position(|d| !d.running()) {
                self.list.remove(oldest);
            }
        }
        self.list.push(Download {
            guid: params["guid"].as_str().unwrap_or("").to_owned(),
            name: params["suggestedFilename"].as_str().unwrap_or("a file").to_owned(),
            received: 0,
            total: None,
            state: Saving::Running,
            seq,
        });
    }

    /// Chrome's `Browser.downloadProgress`. Returns what to record when the
    /// download has just finished, saved or not.
    pub fn progressed(&mut self, params: &Value) -> Option<String> {
        let download = self.list.iter_mut().find(|d| params["guid"] == d.guid.as_str())?;
        if !download.running() {
            return None;
        }
        download.received = params["receivedBytes"].as_u64().unwrap_or(download.received);
        download.total = params["totalBytes"].as_u64().filter(|total| *total > 0);
        download.state = match params["state"].as_str() {
            Some("completed") => {
                Saving::Saved(params["filePath"].as_str().unwrap_or("the downloads folder").to_owned())
            }
            Some("canceled") => Saving::Failed,
            _ => return None,
        };
        Some(download.line())
    }

    /// What to add to the answer of a command whose page messages began at
    /// `seq`: the downloads it set off.
    pub fn since(&self, seq: u64) -> impl Iterator<Item = String> + '_ {
        self.list.iter().filter(move |d| d.seq >= seq).map(|d| match d.running() {
            true => format!("{} (kb ctl status shows how it goes, kb ctl console where it was saved)", d.line()),
            false => d.line(),
        })
    }

    /// A line for each download still running, for `status`.
    pub fn running(&self) -> impl Iterator<Item = String> + '_ {
        self.list.iter().filter(|d| d.running()).map(Download::line)
    }

    /// What the bar shows while files download: "Downloading 4.2 of 20 MB".
    pub fn bar(&self) -> Option<String> {
        let running: Vec<&Download> = self.list.iter().filter(|d| d.running()).collect();
        let received = running.iter().map(|d| d.received).sum();
        let total = running.iter().map(|d| d.total).sum::<Option<u64>>();
        match running.len() {
            0 => None,
            1 => Some(format!("Downloading {}", progress(received, total))),
            n => Some(format!("Downloading {n} files, {}", progress(received, total))),
        }
    }

    /// Mark the downloads still running as given up, for quitting: their
    /// ids, to cancel, and their names, to say so.
    pub fn abandon(&mut self) -> Vec<(String, String)> {
        let running = self.list.iter_mut().filter(|d| d.running());
        running
            .map(|d| {
                d.state = Saving::Failed;
                (d.guid.clone(), d.name.clone())
            })
            .collect()
    }
}

/// A size as people read it: "11 bytes", "240 KB", "4.2 MB".
fn size(bytes: u64) -> String {
    match unit(bytes) {
        (_, "bytes") if bytes == 1 => "1 byte".to_owned(),
        (_, "bytes") => format!("{bytes} bytes"),
        (scale, name) => format!("{} {name}", amount(bytes as f64 / scale)),
    }
}

/// How far a download has come: "4.2 of 20 MB", or "4.2 MB" when the
/// size isn't known.
fn progress(received: u64, total: Option<u64>) -> String {
    match total.filter(|total| *total > 0) {
        Some(total) => {
            let (scale, name) = unit(total);
            if name == "bytes" {
                return format!("{received} of {total} bytes");
            }
            format!("{:.1} of {} {name}", received as f64 / scale, amount(total as f64 / scale))
        }
        None => size(received),
    }
}

/// Decimal units, as the macOS Finder counts.
fn unit(bytes: u64) -> (f64, &'static str) {
    match bytes {
        0..1_000 => (1.0, "bytes"),
        1_000..1_000_000 => (1e3, "KB"),
        1_000_000..1_000_000_000 => (1e6, "MB"),
        _ => (1e9, "GB"),
    }
}

/// One decimal for small amounts, none for large: "4.2", "20".
fn amount(value: f64) -> String {
    if value < 10.0 {
        format!("{value:.1}")
    } else {
        format!("{value:.0}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn sizes_read_as_people_say_them() {
        assert_eq!(size(1), "1 byte");
        assert_eq!(size(11), "11 bytes");
        assert_eq!(size(240_000), "240 KB");
        assert_eq!(size(4_200_000), "4.2 MB");
        assert_eq!(size(1_300_000_000), "1.3 GB");
        assert_eq!(progress(4_200_000, Some(20_000_000)), "4.2 of 20 MB");
        assert_eq!(progress(400_000, Some(1_000_000)), "0.4 of 1.0 MB");
        assert_eq!(progress(5, Some(11)), "5 of 11 bytes");
        assert_eq!(progress(4_200_000, None), "4.2 MB");
        assert_eq!(progress(10, Some(0)), "10 bytes");
    }

    #[test]
    fn a_download_is_followed_to_its_end() {
        let mut downloads = Downloads::default();
        downloads.began(&json!({"guid": "g1", "suggestedFilename": "big.bin"}), 5);
        let step = json!({"guid": "g1", "receivedBytes": 4_200_000, "totalBytes": 20_000_000, "state": "inProgress"});
        assert_eq!(downloads.progressed(&step), None);
        assert_eq!(downloads.bar().as_deref(), Some("Downloading 4.2 of 20 MB"));
        assert_eq!(downloads.running().collect::<Vec<_>>(), ["downloading big.bin, 4.2 of 20 MB so far"]);
        assert_eq!(
            downloads.since(5).collect::<Vec<_>>(),
            ["downloading big.bin, 4.2 of 20 MB so far \
              (kb ctl status shows how it goes, kb ctl console where it was saved)"]
        );
        // A command that came after it doesn't take credit for it.
        assert_eq!(downloads.since(6).count(), 0);

        downloads.began(&json!({"guid": "g2", "suggestedFilename": "report.txt"}), 6);
        assert_eq!(downloads.bar().as_deref(), Some("Downloading 2 files, 4.2 MB"));
        let done = json!({
            "guid": "g2", "receivedBytes": 11, "totalBytes": 11, "state": "completed",
            "filePath": "/Users/me/Downloads/report (1).txt"
        });
        let saved = "downloaded report.txt (11 bytes) to /Users/me/Downloads/report (1).txt";
        assert_eq!(downloads.progressed(&done).as_deref(), Some(saved));
        assert_eq!(downloads.since(6).collect::<Vec<_>>(), [saved]);

        let cancelled = json!({"guid": "g1", "receivedBytes": 0, "state": "canceled"});
        assert_eq!(
            downloads.progressed(&cancelled).as_deref(),
            Some("the download of big.bin failed or was cancelled")
        );
        assert_eq!(downloads.bar(), None);
        // Chrome's progress after the end changes nothing.
        assert_eq!(downloads.progressed(&json!({"guid": "g1", "state": "completed"})), None);
        assert_eq!(downloads.progressed(&json!({"guid": "unknown", "state": "completed"})), None);
    }

    #[test]
    fn quitting_gives_up_only_what_runs() {
        let mut downloads = Downloads::default();
        downloads.began(&json!({"guid": "done", "suggestedFilename": "a.txt"}), 1);
        downloads.progressed(&json!({"guid": "done", "receivedBytes": 1, "state": "completed", "filePath": "/a.txt"}));
        downloads.began(&json!({"guid": "slow", "suggestedFilename": "slow.bin"}), 2);
        assert_eq!(downloads.abandon(), [("slow".to_owned(), "slow.bin".to_owned())]);
        assert_eq!(downloads.abandon(), []);
        assert_eq!(downloads.bar(), None);
    }

    #[test]
    fn only_the_last_finished_downloads_are_kept() {
        let mut downloads = Downloads::default();
        downloads.began(&json!({"guid": "running", "suggestedFilename": "slow.bin"}), 0);
        for n in 0..MAX_FINISHED + 5 {
            let guid = format!("g{n}");
            downloads.began(&json!({"guid": guid, "suggestedFilename": "f"}), n as u64 + 1);
            downloads.progressed(&json!({"guid": guid, "receivedBytes": 1, "state": "completed", "filePath": "/f"}));
        }
        assert_eq!(downloads.list.len(), MAX_FINISHED + 1);
        assert_eq!(downloads.list[0].guid, "running");
    }
}
