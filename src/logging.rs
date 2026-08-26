//! Logging — to the console and a file at once, **one file per day**.
//!
//! This tool runs on **someone else's PC**. Started from the tray, stderr goes nowhere, and
//! then a report of "clicking does not work" leaves nothing to look at. So
//! `logs/deescreen-YYYY-MM-DD.log` is written inside the home directory — see
//! [`crate::config::home`], which is beside the exe in every case but a `cargo install`.
//!
//! ## Why per day
//!
//! Reports arrive shaped like "around three yesterday". Piled into one file, finding that
//! moment means scanning tens of megabytes; split by date, which file to open is already
//! decided. The name uses **local time** — both the person reporting and the person reading
//! speak in terms of the clock on the wall at that PC.
//!
//! Size-based rotation was removed. With one file per day the name is the time axis, and a
//! `.1` generation wedged into that blurs what "the last 30 days" even counts.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use log::{Level, LevelFilter, Metadata, Record};

/// How many dated files to keep. Past this, **the oldest dates** go first.
const KEEP_DAYS: usize = 30;
const PREFIX: &str = "deescreen-";
const SUFFIX: &str = ".log";
/// `deescreen-YYYY-MM-DD.log`
const NAME_LEN: usize = PREFIX.len() + 10 + SUFFIX.len();

type Date = (i32, u8, u8);

/// The file currently open and the date it covers. When the date rolls over, both are replaced.
struct Current {
    date: Date,
    file: File,
}

struct DualLogger {
    dir: PathBuf,
    current: Mutex<Option<Current>>,
    level: LevelFilter,
}

impl DualLogger {
    /// Open that date's file, and prune **only when one was newly created**. There is no
    /// reason to scan the folder per line — the moment a file appears is the moment the count
    /// can overflow.
    fn open_for(&self, date: Date) -> Option<File> {
        let _ = std::fs::create_dir_all(&self.dir);
        let path = self.dir.join(name_for(date));
        let is_new = !path.exists();
        let file = OpenOptions::new().create(true).append(true).open(&path).ok()?;
        if is_new {
            prune(&self.dir);
        }
        Some(file)
    }
}

impl log::Log for DualLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= self.level
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        // Read the clock **once**. The timestamp on the line and the name of the file have
        // to come from the same instant, or around midnight you get lines like "today's time
        // in yesterday's file".
        let now = local_now();
        let line = format!(
            "{} {:<5} [{}] {}",
            stamp(&now),
            record.level(),
            record.target(),
            record.args()
        );
        // console — when started interactively
        if record.level() <= Level::Warn {
            eprintln!("{line}");
        } else {
            println!("{line}");
        }
        // file
        if let Ok(mut guard) = self.current.lock() {
            let today = (now.year(), now.month() as u8, now.day());
            if guard.as_ref().map(|c| c.date) != Some(today) {
                *guard = None; // close yesterday's file first
                if let Some(file) = self.open_for(today) {
                    *guard = Some(Current { date: today, file });
                }
            }
            if let Some(c) = guard.as_mut() {
                let _ = writeln!(c.file, "{line}");
                let _ = c.file.flush();
            }
        }
    }

    fn flush(&self) {
        if let Ok(mut g) = self.current.lock()
            && let Some(c) = g.as_mut()
        {
            let _ = c.file.flush();
        }
    }
}

fn name_for(date: Date) -> String {
    format!("{PREFIX}{:04}-{:02}-{:02}{SUFFIX}", date.0, date.1, date.2)
}

/// Whether this is one of our dated files — judged strictly, from the name alone.
///
/// This judgement **is the delete decision.** Left loose, a note or a backup someone put in
/// the same folder could be swept away by the count of 30, so it checks each position is
/// actually a digit.
fn is_dated_log(name: &str) -> bool {
    if name.len() != NAME_LEN || !name.starts_with(PREFIX) || !name.ends_with(SUFFIX) {
        return false;
    }
    let d = &name[PREFIX.len()..PREFIX.len() + 10];
    let b = d.as_bytes();
    b[4] == b'-' && b[7] == b'-' && [0, 1, 2, 3, 5, 6, 8, 9].iter().all(|&i| b[i].is_ascii_digit())
}

/// Past 30, delete oldest-date-first.
fn prune(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut logs: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name().and_then(|n| n.to_str()).map(is_dated_log).unwrap_or(false)
        })
        .collect();
    if logs.len() <= KEEP_DAYS {
        return;
    }
    // The name is the date — ISO, so alphabetical order is chronological order. File times
    // (mtime) are easily disturbed by copying, backup and virus scanning, so trust the name.
    logs.sort();
    for old in &logs[..logs.len() - KEEP_DAYS] {
        let _ = std::fs::remove_file(old);
    }
}

/// Local time. If the offset cannot be determined this falls back to UTC — and then the file
/// name and the line timestamps are **both** UTC, so at least they still agree with each other.
pub(crate) fn local_now() -> time::OffsetDateTime {
    time::OffsetDateTime::now_local().unwrap_or_else(|_| time::OffsetDateTime::now_utc())
}

fn stamp(now: &time::OffsetDateTime) -> String {
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}",
        now.year(),
        now.month() as u8,
        now.day(),
        now.hour(),
        now.minute(),
        now.second(),
        now.millisecond()
    )
}

/// The path of today's log file, so the startup log can say where to look.
pub fn today_path(dir: &Path) -> PathBuf {
    let now = local_now();
    dir.join(name_for((now.year(), now.month() as u8, now.day())))
}

/// `RUST_LOG` sets the level (`error|warn|info|debug|trace`). Defaults to `info`.
pub fn init(dir: PathBuf) {
    let level = match std::env::var("RUST_LOG").unwrap_or_default().to_lowercase().as_str() {
        "error" => LevelFilter::Error,
        "warn" => LevelFilter::Warn,
        "debug" => LevelFilter::Debug,
        "trace" => LevelFilter::Trace,
        _ => LevelFilter::Info,
    };
    // Prune once at startup. After a few days off, a new file appears and prunes anyway —
    // but on a PC restarted several times in one day that moment never comes.
    let _ = std::fs::create_dir_all(&dir);
    prune(&dir);
    let logger = DualLogger { dir, current: Mutex::new(None), level };
    log::set_max_level(level);
    let _ = log::set_boxed_logger(Box::new(logger));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_our_dated_files_are_deletable() {
        assert!(is_dated_log("deescreen-2026-08-26.log"));

        // Things a person put in the same folder — the count of 30 must not sweep them up
        assert!(!is_dated_log("deescreen.log")); // no date in the name
        assert!(!is_dated_log("deescreen-2026-08-26.log.1")); // an old rotation generation
        assert!(!is_dated_log("deescreen-backup-old.log")); // same length, but not digits
        assert!(!is_dated_log("notes.txt"));
        assert!(!is_dated_log("deescreen-2026-08-26.txt"));
        assert!(!is_dated_log("prefix-deescreen-2026-08-26.log"));
    }

    #[test]
    fn names_are_zero_padded_so_they_sort_by_date() {
        assert_eq!(name_for((2026, 8, 6)), "deescreen-2026-08-06.log");
        // alphabetical order has to be chronological for "oldest first" to hold
        let mut v = [name_for((2026, 12, 1)), name_for((2026, 2, 10)), name_for((2025, 12, 31))];
        v.sort();
        assert_eq!(v[0], "deescreen-2025-12-31.log");
        assert_eq!(v[2], "deescreen-2026-12-01.log");
    }

    #[test]
    fn prune_keeps_the_newest_thirty_and_spares_strangers() {
        let dir = std::env::temp_dir().join(format!("deescreen-prune-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");

        // 40 days' worth, plus one file that is not ours
        for day in 1..=40u8 {
            std::fs::write(dir.join(name_for((2026, 1, day))), b"x").expect("write");
        }
        std::fs::write(dir.join("keep-me.txt"), b"x").expect("write");

        prune(&dir);

        let mut left: Vec<String> = std::fs::read_dir(&dir)
            .expect("read")
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(left.len(), KEEP_DAYS + 1, "{left:?}");
        assert!(left.contains(&"keep-me.txt".to_string()));
        assert!(left.contains(&name_for((2026, 1, 11)))); // 40 - 30 + 1 = day 11 onwards survives
        assert!(!left.contains(&name_for((2026, 1, 10))));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
