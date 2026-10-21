//! Turns a chosen slice into a worker run, and owns the output naming rules (SPEC 4.2).
//!
//! Workers always write to a temp file in the destination folder. Only after a successful
//! response is it moved to its real name, without ever replacing an existing file. On any
//! failure the temp file is deleted, so a crash never leaves a half-written file behind.

use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering as AtomicOrdering};

/// Sorts paths by file name the way people expect: case-insensitive, with numbers compared by
/// value, so "Chapter 2" comes before "Chapter 10".
pub fn sort_by_name(paths: &mut [PathBuf]) {
    paths.sort_by(|a, b| natural_cmp(&file_name(a), &file_name(b)));
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (mut a, mut b) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (a.peek().copied(), b.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let take = |it: &mut std::iter::Peekable<std::str::Chars>| {
                    let mut digits = String::new();
                    while let Some(c) = it.peek().copied().filter(char::is_ascii_digit) {
                        digits.push(c);
                        it.next();
                    }
                    digits
                };
                let (da, db) = (take(&mut a), take(&mut b));
                let (ta, tb) = (da.trim_start_matches('0'), db.trim_start_matches('0'));
                let order = ta.len().cmp(&tb.len()).then_with(|| ta.cmp(tb));
                if order != Ordering::Equal {
                    return order;
                }
            }
            (Some(x), Some(y)) => {
                if x != y {
                    return x.cmp(&y);
                }
                a.next();
                b.next();
            }
        }
    }
}

/// `report.pdf` stays `report.pdf` if free, otherwise `report (1).pdf`, `report (2).pdf`, ...
pub fn free_name(desired: &Path, exists: impl Fn(&Path) -> bool) -> PathBuf {
    if !exists(desired) {
        return desired.to_path_buf();
    }
    let stem = desired
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = desired
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    (1..)
        .map(|n| desired.with_file_name(format!("{stem} ({n}){ext}")))
        .find(|candidate| !exists(candidate))
        .expect("some name is free")
}

/// The name a merge result should get: named after the first input, next to it.
pub fn merged_name(sorted_inputs: &[PathBuf]) -> Option<PathBuf> {
    let first = sorted_inputs.first()?;
    let stem = first.file_stem()?.to_string_lossy();
    Some(first.with_file_name(format!("{stem} - merged.pdf")))
}

/// A unique, clearly-temporary sibling of `final_path`, on the same volume so the final move is
/// a rename rather than a copy.
pub fn temp_path_for(final_path: &Path) -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, AtomicOrdering::Relaxed);
    let name = final_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    final_path.with_file_name(format!("~{name}.{}-{n}.flick-tmp", std::process::id()))
}

/// Moves `temp` to `desired`, or to the next free `(n)` name if something appeared there in the
/// meantime. Never replaces an existing file. Returns the final path.
pub fn commit(temp: &Path, desired: &Path) -> std::io::Result<PathBuf> {
    for _ in 0..1000 {
        let target = free_name(desired, |p| p.exists());
        match move_no_replace(temp, &target) {
            Ok(()) => return Ok(target),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::other("no free output name"))
}

#[cfg(windows)]
fn move_no_replace(from: &Path, to: &Path) -> std::io::Result<()> {
    use windows::core::HSTRING;
    use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, ERROR_FILE_EXISTS};
    use windows::Win32::Storage::FileSystem::{MoveFileExW, MOVEFILE_WRITE_THROUGH};

    // Without MOVEFILE_REPLACE_EXISTING the move fails if the target exists.
    unsafe {
        MoveFileExW(
            &HSTRING::from(from),
            &HSTRING::from(to),
            MOVEFILE_WRITE_THROUGH,
        )
    }
    .map_err(|e| {
        if e.code() == ERROR_ALREADY_EXISTS.to_hresult()
            || e.code() == ERROR_FILE_EXISTS.to_hresult()
        {
            std::io::Error::from(std::io::ErrorKind::AlreadyExists)
        } else {
            std::io::Error::other(e)
        }
    })
}

#[cfg(not(windows))]
fn move_no_replace(from: &Path, to: &Path) -> std::io::Result<()> {
    // hard_link fails if `to` exists, which gives the same no-replace guarantee.
    std::fs::hard_link(from, to)?;
    std::fs::remove_file(from)
}

#[cfg(windows)]
pub use run::{merge_pdfs, JobError};

#[cfg(windows)]
mod run {
    use std::path::{Path, PathBuf};

    use serde::Deserialize;
    use serde_json::json;

    use super::{commit, merged_name, sort_by_name, temp_path_for};
    use crate::supervisor::{self, CancelToken, Limits, SupervisorError, Worker};

    #[derive(Debug, thiserror::Error)]
    pub enum JobError {
        #[error(transparent)]
        Supervisor(#[from] SupervisorError),
        /// The worker ran and explained what was wrong, e.g. a password-protected input.
        #[error("{0}")]
        Worker(String),
        #[error("Could not save the result ({0}).")]
        Save(std::io::Error),
        #[error("{0}")]
        BadInput(&'static str),
    }

    #[derive(Deserialize)]
    struct Response {
        ok: bool,
        #[serde(default)]
        error: Option<WorkerError>,
    }

    #[derive(Deserialize)]
    struct WorkerError {
        message: String,
    }

    /// Merges PDFs (sorted by name) into `<first> - merged.pdf` next to the first one.
    pub fn merge_pdfs(
        worker_exe: &Path,
        inputs: &[PathBuf],
        limits: &Limits,
        cancel: &CancelToken,
    ) -> Result<PathBuf, JobError> {
        let mut inputs = inputs.to_vec();
        sort_by_name(&mut inputs);
        let desired = merged_name(&inputs).ok_or(JobError::BadInput("Nothing to merge."))?;
        let temp = temp_path_for(&desired);

        let result = run_worker(worker_exe, &inputs, &temp, limits, cancel)
            .and_then(|()| commit(&temp, &desired).map_err(JobError::Save));
        if result.is_err() {
            // The original inputs are never touched; only our own temp file is cleaned up.
            let _ = std::fs::remove_file(&temp);
        }
        result
    }

    fn run_worker(
        worker_exe: &Path,
        inputs: &[PathBuf],
        temp: &Path,
        limits: &Limits,
        cancel: &CancelToken,
    ) -> Result<(), JobError> {
        let worker = Worker {
            exe: worker_exe.to_path_buf(),
            args: Vec::new(),
        };
        let request = json!({ "op": "merge", "inputs": inputs, "output": temp });
        let response: Response = supervisor::run(&worker, &request, limits, cancel)?;
        if response.ok {
            Ok(())
        } else {
            Err(JobError::Worker(
                response
                    .error
                    .map(|e| e.message)
                    .unwrap_or_else(|| "The worker failed without saying why.".into()),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn natural_sort_orders_numbers_by_value() {
        let mut paths: Vec<PathBuf> = [
            "Chapter 10.pdf",
            "chapter 2.pdf",
            "Chapter 1.pdf",
            "Appendix.pdf",
        ]
        .into_iter()
        .map(PathBuf::from)
        .collect();
        sort_by_name(&mut paths);
        let names: Vec<_> = paths.iter().map(|p| p.to_str().unwrap()).collect();
        assert_eq!(
            names,
            [
                "Appendix.pdf",
                "Chapter 1.pdf",
                "chapter 2.pdf",
                "Chapter 10.pdf"
            ]
        );
    }

    #[test]
    fn leading_zeros_do_not_change_the_order() {
        assert_eq!(natural_cmp("page 007", "page 7"), Ordering::Equal);
        assert_eq!(natural_cmp("page 007", "page 8"), Ordering::Less);
    }

    #[test]
    fn collisions_get_numbered() {
        let taken = ["report.docx", "report (1).docx"];
        let exists = |p: &Path| taken.contains(&p.to_str().unwrap());
        assert_eq!(
            free_name(Path::new("report.docx"), exists),
            Path::new("report (2).docx")
        );
        assert_eq!(
            free_name(Path::new("other.docx"), exists),
            Path::new("other.docx")
        );
        assert_eq!(
            free_name(Path::new("README"), |p| p == Path::new("README")),
            Path::new("README (1)")
        );
    }

    #[test]
    fn merge_is_named_after_the_first_file() {
        let inputs = [
            PathBuf::from("dir/Chapter 1.pdf"),
            PathBuf::from("dir/Chapter 2.pdf"),
        ];
        assert_eq!(
            merged_name(&inputs).unwrap(),
            Path::new("dir/Chapter 1 - merged.pdf")
        );
        assert_eq!(merged_name(&[]), None);
    }

    #[test]
    fn commit_never_replaces_an_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let desired = dir.path().join("out.pdf");
        std::fs::write(&desired, b"original").unwrap();

        let temp = temp_path_for(&desired);
        std::fs::write(&temp, b"new").unwrap();
        let final_path = commit(&temp, &desired).unwrap();

        assert_eq!(final_path, dir.path().join("out (1).pdf"));
        assert_eq!(std::fs::read(&desired).unwrap(), b"original");
        assert_eq!(std::fs::read(&final_path).unwrap(), b"new");
        assert!(!temp.exists());
    }
}
