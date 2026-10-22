//! Turns a chosen slice into worker runs, and owns the output naming rules (SPEC 4.2).
//!
//! Workers always write to a temp file (or, for slices that make several files, a temp folder)
//! in the destination folder. Only after a successful response are results moved to their real
//! names, without ever replacing an existing file. On any failure the temp file or folder is
//! deleted, so a crash never leaves a half-written file behind.

use std::cmp::Ordering;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering as AtomicOrdering};

use crate::packs::{Mode, PackSlice};

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

/// Where a slice's result should go: next to the first input (after sorting), named by the
/// slice's output template. The `(n)` collision rule is applied later, at commit time.
pub fn output_path(template: &str, sorted_inputs: &[PathBuf]) -> Result<PathBuf, String> {
    let first = sorted_inputs.first().ok_or("Nothing is selected.")?;
    let name = crate::packs::output_name(template, first, None)?;
    Ok(first.with_file_name(name))
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

/// Why a job (or one run of it) failed.
#[derive(Debug, thiserror::Error)]
pub enum JobError {
    #[cfg(windows)]
    #[error(transparent)]
    Supervisor(crate::supervisor::SupervisorError),
    #[error("Cancelled.")]
    Cancelled,
    /// The worker ran and explained what was wrong, e.g. a password-protected input.
    #[error("{0}")]
    Worker(String),
    #[error("Could not save the result ({0}).")]
    Save(std::io::Error),
    #[error("{0}")]
    BadInput(String),
}

/// Where one worker run writes: a temp file, or for slices that make several files (`{n}` in
/// the output template) an empty temp folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target<'a> {
    File(&'a Path),
    Folder(&'a Path),
}

/// What a finished job saved.
#[derive(Debug, Default)]
pub struct Outcome {
    /// The new files, in input order (and page order within an input).
    pub outputs: Vec<PathBuf>,
    /// How many worker runs succeeded.
    pub done: usize,
    /// Inputs whose run failed, and why. Only `each` jobs with several files get here: there, one
    /// bad file should not stop the others.
    pub failed: Vec<(PathBuf, JobError)>,
}

/// Runs `slice` on `inputs` (sorted by name): once with all of them (`combine`) or once per file
/// (`each`). `run` starts the worker for one run and returns the files it reports; it must write
/// only to the target it is given.
///
/// Every run writes to a temp file or folder next to its first input. Only after a successful run
/// are the results moved to their real names, never replacing a file; after a failure the temp
/// file or folder is deleted. The original inputs are never touched.
pub fn run_slice_with<F>(
    slice: &PackSlice,
    inputs: &[PathBuf],
    mut run: F,
) -> Result<Outcome, JobError>
where
    F: FnMut(&[PathBuf], Target<'_>) -> Result<Vec<PathBuf>, JobError>,
{
    let mut inputs = inputs.to_vec();
    sort_by_name(&mut inputs);
    if inputs.is_empty() {
        return Err(JobError::BadInput("Nothing is selected.".into()));
    }
    let groups: Vec<Vec<PathBuf>> = match slice.mode {
        Mode::Combine => vec![inputs],
        Mode::Each => inputs.into_iter().map(|p| vec![p]).collect(),
    };
    let alone = groups.len() == 1;

    let mut outcome = Outcome::default();
    for group in groups {
        match run_once(slice, &group, &mut run) {
            Ok(saved) => {
                outcome.outputs.extend(saved);
                outcome.done += 1;
            }
            // Files saved by earlier runs are complete, so a cancel keeps them.
            Err(e) if alone || matches!(e, JobError::Cancelled) => return Err(e),
            Err(e) => outcome.failed.push((group[0].clone(), e)),
        }
    }
    if outcome.outputs.is_empty() {
        let count = outcome.failed.len();
        let (input, error) = outcome.failed.remove(0);
        let others = match count - 1 {
            0 => String::new(),
            1 => " 1 other file failed too.".into(),
            n => format!(" {n} other files failed too."),
        };
        return Err(JobError::Worker(format!(
            "{}{others}",
            failure_message(&input, &error)
        )));
    }
    Ok(outcome)
}

fn run_once<F>(slice: &PackSlice, group: &[PathBuf], run: &mut F) -> Result<Vec<PathBuf>, JobError>
where
    F: FnMut(&[PathBuf], Target<'_>) -> Result<Vec<PathBuf>, JobError>,
{
    let first = &group[0];
    let name = |n: Option<&str>| {
        crate::packs::output_name(&slice.output, first, n)
            .map(|name| first.with_file_name(name))
            .map_err(JobError::BadInput)
    };

    if !slice.multi_output() {
        let desired = name(None)?;
        let temp = temp_path_for(&desired);
        let result = run(group, Target::File(&temp))
            .and_then(|_| commit(&temp, &desired).map_err(JobError::Save));
        if result.is_err() {
            let _ = std::fs::remove_file(&temp);
        }
        return result.map(|path| vec![path]);
    }

    let folder = temp_path_for(&name(Some("n"))?);
    std::fs::create_dir(&folder).map_err(JobError::Save)?;
    let result = run(group, Target::Folder(&folder))
        .and_then(|files| commit_numbered(&folder, &files, |n| name(Some(n))));
    // The folder is ours: after a success it holds only leftovers, after a failure everything.
    let _ = std::fs::remove_dir_all(&folder);
    result
}

/// Moves the files a worker wrote into `folder` to their numbered names, in the order the worker
/// reported them. Numbers are zero-padded to the same width ("page 01" to "page 12") so the files
/// sort. If anything goes wrong, the files already moved are removed again: a run saves all of
/// its files or none.
fn commit_numbered(
    folder: &Path,
    files: &[PathBuf],
    name: impl Fn(&str) -> Result<PathBuf, JobError>,
) -> Result<Vec<PathBuf>, JobError> {
    if files.is_empty() {
        return Err(JobError::Worker(
            "The worker did not make any files.".into(),
        ));
    }
    let width = files.len().to_string().len();
    let mut seen = HashSet::new();
    let mut saved = Vec::new();
    for (i, file) in files.iter().enumerate() {
        // Only files directly inside our temp folder are moved, whatever path the worker reports.
        let source = file
            .file_name()
            .filter(|n| seen.insert(n.to_os_string()))
            .map(|n| folder.join(n))
            .filter(|p| p.is_file());
        let step = match source {
            Some(source) => name(&format!("{:0width$}", i + 1))
                .and_then(|desired| commit(&source, &desired).map_err(JobError::Save)),
            None => Err(JobError::Worker(format!(
                "The worker reported a file it did not make ({}).",
                file.display()
            ))),
        };
        match step {
            Ok(path) => saved.push(path),
            Err(e) => {
                for path in &saved {
                    let _ = std::fs::remove_file(path);
                }
                return Err(e);
            }
        }
    }
    Ok(saved)
}

/// The error for one input, naming the file unless the worker's message already does.
fn failure_message(input: &Path, error: &JobError) -> String {
    let message = error.to_string();
    let name = input
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if name.is_empty() || message.contains(&name) {
        message
    } else {
        format!("{name}: {message}")
    }
}

/// What the toast says about a finished job.
#[derive(Debug, PartialEq, Eq)]
pub enum Summary {
    /// Everything worked; `output` is the (first) new file, for Open and Show in folder.
    Done { title: String, output: PathBuf },
    /// Some files failed while others were saved.
    Partial { title: String, message: String },
}

pub fn summarize(label: &str, outcome: &Outcome) -> Summary {
    match outcome.failed.first() {
        None => Summary::Done {
            title: match outcome.outputs.len() {
                1 => format!("{label} done"),
                n => format!("{label} done: {n} files"),
            },
            output: outcome.outputs.first().cloned().unwrap_or_default(),
        },
        Some((input, error)) => Summary::Partial {
            title: format!(
                "{label}: {} of {} failed",
                outcome.failed.len(),
                outcome.failed.len() + outcome.done
            ),
            message: format!(
                "{} {}",
                failure_message(input, error),
                match outcome.outputs.len() {
                    1 => "1 file was saved.".to_string(),
                    n => format!("{n} files were saved."),
                }
            ),
        },
    }
}

#[cfg(windows)]
pub use run::run_slice;

#[cfg(windows)]
mod run {
    use std::path::PathBuf;
    use std::time::Duration;

    use serde::Deserialize;
    use serde_json::json;

    use super::{run_slice_with, JobError, Outcome, Target};
    use crate::packs::{Pack, PackSlice};
    use crate::supervisor::{self, CancelToken, Limits, SupervisorError, Worker};

    #[derive(Deserialize)]
    struct Response {
        ok: bool,
        /// Files written into the output folder, for slices that make several files.
        #[serde(default)]
        outputs: Vec<PathBuf>,
        #[serde(default)]
        error: Option<WorkerError>,
    }

    #[derive(Deserialize)]
    struct WorkerError {
        message: String,
    }

    /// Runs `slice` on `inputs` with `pack`'s worker. Every worker run gets its own Job Object
    /// with the pack's limits. See [`run_slice_with`] for where results are saved.
    pub fn run_slice(
        pack: &Pack,
        slice: &PackSlice,
        inputs: &[PathBuf],
        cancel: &CancelToken,
    ) -> Result<Outcome, JobError> {
        let worker = Worker {
            exe: pack.worker_exe.clone(),
            args: Vec::new(),
        };
        let limits = Limits {
            memory_mb: pack.limits.memory_mb,
            cpu_percent: pack.limits.cpu_percent,
            time: Duration::from_secs(pack.limits.time_seconds),
        };
        run_slice_with(slice, inputs, |group, target| {
            if cancel.is_cancelled() {
                return Err(JobError::Cancelled);
            }
            let mut request = json!({ "op": slice.op, "inputs": group });
            match target {
                Target::File(path) => request["output"] = json!(path),
                Target::Folder(path) => request["output_dir"] = json!(path),
            }
            let response: Response =
                supervisor::run(&worker, &request, &limits, cancel).map_err(|e| match e {
                    SupervisorError::Cancelled => JobError::Cancelled,
                    e => JobError::Supervisor(e),
                })?;
            if response.ok {
                Ok(response.outputs)
            } else {
                Err(JobError::Worker(
                    response
                        .error
                        .map(|e| e.message)
                        .unwrap_or_else(|| "The worker failed without saying why.".into()),
                ))
            }
        })
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
    fn output_goes_next_to_the_first_input() {
        let inputs = [
            PathBuf::from("dir/Chapter 1.pdf"),
            PathBuf::from("dir/Chapter 2.pdf"),
        ];
        assert_eq!(
            output_path("{stem} - merged.pdf", &inputs).unwrap(),
            Path::new("dir/Chapter 1 - merged.pdf")
        );
        assert!(output_path("{stem}.pdf", &[]).is_err());
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

    fn slice(mode: Mode, output: &str) -> PackSlice {
        PackSlice {
            id: "p.s".into(),
            label: "Split".into(),
            icon: None,
            applies_to: vec!["*".into()],
            min_files: 1,
            max_files: None,
            mode,
            output: output.into(),
            worker: "p".into(),
            op: "s".into(),
        }
    }

    /// Input files `names` in a new temp folder.
    fn inputs(names: &[&str]) -> (tempfile::TempDir, Vec<PathBuf>) {
        let dir = tempfile::tempdir().unwrap();
        let paths = names
            .iter()
            .map(|name| {
                let path = dir.path().join(name);
                std::fs::write(&path, name.as_bytes()).unwrap();
                path
            })
            .collect();
        (dir, paths)
    }

    /// Names of the files in `dir`, sorted.
    fn listing(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    /// A worker stand-in that writes `pages` numbered files into the folder it is given.
    fn split_into(
        pages: usize,
    ) -> impl FnMut(&[PathBuf], Target<'_>) -> Result<Vec<PathBuf>, JobError> {
        move |_, target| {
            let Target::Folder(folder) = target else {
                panic!("expected a folder, got {target:?}");
            };
            Ok((1..=pages)
                .map(|n| {
                    let path = folder.join(format!("{n}.pdf"));
                    std::fs::write(&path, format!("page {n}")).unwrap();
                    path
                })
                .collect())
        }
    }

    #[test]
    fn combine_runs_once_with_every_input() {
        let (dir, paths) = inputs(&["b.pdf", "a.pdf"]);
        let mut runs = Vec::new();
        let outcome = run_slice_with(
            &slice(Mode::Combine, "{stem} - merged.pdf"),
            &paths,
            |group, target| {
                runs.push(group.to_vec());
                let Target::File(temp) = target else { panic!() };
                std::fs::write(temp, b"merged").unwrap();
                Ok(Vec::new())
            },
        )
        .unwrap();

        assert_eq!(
            runs,
            [vec![dir.path().join("a.pdf"), dir.path().join("b.pdf")]]
        );
        assert_eq!(outcome.outputs, [dir.path().join("a - merged.pdf")]);
        assert_eq!(listing(dir.path()), ["a - merged.pdf", "a.pdf", "b.pdf"]);
    }

    #[test]
    fn each_runs_once_per_input_and_saves_next_to_it() {
        let (dir, paths) = inputs(&["b.png", "a.png"]);
        std::fs::write(dir.path().join("a.jpg"), b"taken").unwrap();
        let outcome = run_slice_with(&slice(Mode::Each, "{stem}.jpg"), &paths, |group, target| {
            assert_eq!(group.len(), 1);
            let Target::File(temp) = target else { panic!() };
            std::fs::write(temp, b"jpg").unwrap();
            Ok(Vec::new())
        })
        .unwrap();

        assert_eq!(
            outcome.outputs,
            [dir.path().join("a (1).jpg"), dir.path().join("b.jpg")]
        );
        assert_eq!(std::fs::read(dir.path().join("a.jpg")).unwrap(), b"taken");
        assert_eq!(
            summarize("To JPG", &outcome),
            Summary::Done {
                title: "To JPG done: 2 files".into(),
                output: dir.path().join("a (1).jpg"),
            }
        );
    }

    #[test]
    fn each_keeps_going_after_one_file_fails() {
        let (dir, paths) = inputs(&["a.png", "b.png", "c.png"]);
        let outcome = run_slice_with(
            &slice(Mode::Each, "{stem} - small.jpg"),
            &paths,
            |group, target| {
                let Target::File(temp) = target else { panic!() };
                std::fs::write(temp, b"partial").unwrap();
                if group[0].ends_with("b.png") {
                    return Err(JobError::Worker("It would not get smaller.".into()));
                }
                Ok(Vec::new())
            },
        )
        .unwrap();

        assert_eq!(outcome.done, 2);
        assert_eq!(
            listing(dir.path()),
            ["a - small.jpg", "a.png", "b.png", "c - small.jpg", "c.png"]
        );
        assert_eq!(
            summarize("Shrink", &outcome),
            Summary::Partial {
                title: "Shrink: 1 of 3 failed".into(),
                message: "b.png: It would not get smaller. 2 files were saved.".into(),
            }
        );
    }

    #[test]
    fn a_lone_failure_is_the_jobs_error_and_leaves_nothing_behind() {
        let (dir, paths) = inputs(&["a.png"]);
        let err = run_slice_with(&slice(Mode::Each, "{stem}.jpg"), &paths, |_, target| {
            let Target::File(temp) = target else { panic!() };
            std::fs::write(temp, b"half").unwrap();
            Err(JobError::Worker("a.png could not be read.".into()))
        })
        .unwrap_err();
        assert_eq!(err.to_string(), "a.png could not be read.");
        assert_eq!(listing(dir.path()), ["a.png"]);

        // When every file fails, the first reason is shown and the rest are counted.
        let (dir, paths) = inputs(&["a.png", "b.png", "c.png"]);
        let err = run_slice_with(&slice(Mode::Each, "{stem}.jpg"), &paths, |_, _| {
            Err(JobError::Worker("Not an image.".into()))
        })
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            "a.png: Not an image. 2 other files failed too."
        );
        assert_eq!(listing(dir.path()), ["a.png", "b.png", "c.png"]);
    }

    #[test]
    fn cancel_stops_an_each_job() {
        let (_dir, paths) = inputs(&["a.png", "b.png", "c.png"]);
        let mut runs = 0;
        let err = run_slice_with(&slice(Mode::Each, "{stem}.jpg"), &paths, |_, target| {
            runs += 1;
            if runs == 2 {
                return Err(JobError::Cancelled);
            }
            let Target::File(temp) = target else { panic!() };
            std::fs::write(temp, b"jpg").unwrap();
            Ok(Vec::new())
        })
        .unwrap_err();
        assert!(matches!(err, JobError::Cancelled));
        assert_eq!(runs, 2);
    }

    #[test]
    fn multi_output_files_are_numbered_and_padded() {
        let (dir, paths) = inputs(&["Report.pdf"]);
        // "Report - page 03.pdf" is taken, so that one becomes "(1)".
        std::fs::write(dir.path().join("Report - page 03.pdf"), b"keep").unwrap();
        let outcome = run_slice_with(
            &slice(Mode::Each, "{stem} - page {n}.pdf"),
            &paths,
            split_into(12),
        )
        .unwrap();

        assert_eq!(outcome.outputs.len(), 12);
        assert_eq!(outcome.outputs[0], dir.path().join("Report - page 01.pdf"));
        assert_eq!(
            outcome.outputs[2],
            dir.path().join("Report - page 03 (1).pdf")
        );
        assert_eq!(outcome.outputs[11], dir.path().join("Report - page 12.pdf"));
        assert_eq!(std::fs::read(&outcome.outputs[11]).unwrap(), b"page 12");
        assert_eq!(
            std::fs::read(dir.path().join("Report - page 03.pdf")).unwrap(),
            b"keep"
        );
        // 1 input + 1 existing file + 12 pages, and no temp folder left.
        assert_eq!(listing(dir.path()).len(), 14);
        assert_eq!(
            summarize("Split", &outcome),
            Summary::Done {
                title: "Split done: 12 files".into(),
                output: dir.path().join("Report - page 01.pdf"),
            }
        );

        // Under ten files there is no padding.
        let (dir, paths) = inputs(&["a.pdf"]);
        run_slice_with(
            &slice(Mode::Each, "{stem} - page {n}.pdf"),
            &paths,
            split_into(2),
        )
        .unwrap();
        assert_eq!(
            listing(dir.path()),
            ["a - page 1.pdf", "a - page 2.pdf", "a.pdf"]
        );
    }

    #[test]
    fn multi_output_failures_leave_nothing_behind() {
        let template = "{stem} - page {n}.pdf";
        // The worker fails after writing some files.
        let (dir, paths) = inputs(&["a.pdf"]);
        let mut half = split_into(3);
        let err = run_slice_with(&slice(Mode::Each, template), &paths, |group, target| {
            half(group, target)?;
            Err(JobError::Worker("a.pdf is password protected.".into()))
        })
        .unwrap_err();
        assert_eq!(err.to_string(), "a.pdf is password protected.");
        assert_eq!(listing(dir.path()), ["a.pdf"]);

        // The worker reports a file outside its folder, after real ones: nothing is kept.
        let (dir, paths) = inputs(&["a.pdf", "elsewhere.pdf"]);
        let outside = paths[1].clone();
        let mut pages = split_into(2);
        let err = run_slice_with(
            &slice(Mode::Combine, template),
            &paths[..1],
            |group, target| {
                let mut files = pages(group, target)?;
                files.push(outside.clone());
                Ok(files)
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("did not make"), "{err}");
        assert_eq!(listing(dir.path()), ["a.pdf", "elsewhere.pdf"]);

        // No files at all is an error too.
        let (dir, paths) = inputs(&["a.pdf"]);
        let err = run_slice_with(&slice(Mode::Each, template), &paths, |_, _| Ok(Vec::new()))
            .unwrap_err();
        assert!(err.to_string().contains("did not make any files"), "{err}");
        assert_eq!(listing(dir.path()), ["a.pdf"]);
    }
}
