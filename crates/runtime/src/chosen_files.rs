//! Files a person chose in a dialog, held by token rather than by path.
//!
//! Every other Krate grant is decided before the app runs, from a list somebody
//! reads. This one is decided during the run, from a click -- and the click is
//! the grant. Choosing a file in a native dialog says "this app may have this
//! file" more directly than any manifest line.
//!
//! The app is handed a token and a display name, never a path. That is the
//! whole point: a path would let it read the chosen file's siblings, walk up to
//! its folder, or store the location and come back on a later run for a file
//! nobody offered again. A token buys one file, for one run.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// How many chosen files one run may hold at once.
///
/// A person clicking through a dialog will not reach this. A loop calling the
/// picker would, and a bound is what stops one run accumulating handles until
/// memory runs out.
const MAX_CHOSEN_FILES: usize = 64;

/// The files this run may open, keyed by the token the app was given.
#[derive(Debug, Default)]
pub struct ChosenFiles {
    by_token: HashMap<String, PathBuf>,
    /// Folders picked in a dialog. A folder token grants the subtree under
    /// it -- reached through `picked/<token>/...` paths -- for this run
    /// only, which is the answer to "a tidier cannot name a folder" (K-075):
    /// the person names it by picking, and the pick is the grant.
    folders_by_token: HashMap<String, PathBuf>,
    /// Files a save dialog handed out on a phone, where a save is a file
    /// written in Krate first and then handed to the system to place: the
    /// app writes it, and once the app lets go of it, it goes to the
    /// system's save sheet (K-1032).
    exports: std::collections::HashSet<PathBuf>,
    /// Export files open for writing now, by the app's file handle.
    writing: HashMap<u32, PathBuf>,
    /// Export files written and let go of, waiting for the save sheet.
    ready: Vec<PathBuf>,
    /// Counts every token ever issued in this run, so a token is never reused
    /// even after one is dropped.
    issued: u64,
}

impl ChosenFiles {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a chosen file and return the token the app will use.
    ///
    /// Returns `None` when this run already holds the maximum.
    pub fn remember(&mut self, path: PathBuf) -> Option<String> {
        // The SUM, not just the files (K-389). `remember_folder` has always
        // counted both, and the comment there calls this one budget -- but
        // this side counted only files, so filling with folders first and
        // then files gave 64 of each: twice the bound the constant exists
        // to enforce.
        if self.held() >= MAX_CHOSEN_FILES {
            return None;
        }
        self.issued += 1;
        // Not a random token: it never leaves this process, and a counter makes
        // a stale one obviously stale rather than plausibly valid. It carries
        // no part of the path, so it tells the app nothing about where the file
        // is -- which is the property that matters.
        let token = format!("chosen-{}", self.issued);
        self.by_token.insert(token.clone(), path);
        Some(token)
    }

    /// Record a chosen folder and return the token the app will use.
    /// Shares the run-wide bound with files: one budget for everything a
    /// person can be asked to click through.
    pub fn remember_folder(&mut self, path: PathBuf) -> Option<String> {
        if self.held() >= MAX_CHOSEN_FILES {
            return None;
        }
        self.issued += 1;
        let token = format!("folder-{}", self.issued);
        self.folders_by_token.insert(token.clone(), path);
        Some(token)
    }

    /// Record a file a phone's save will hand to the system once written,
    /// and return its token, as `remember` does.
    pub fn remember_export(&mut self, path: PathBuf) -> Option<String> {
        let token = self.remember(path.clone())?;
        self.exports.insert(path);
        Some(token)
    }

    /// An app opened `path` for writing as handle `handle`: when it lets
    /// go, an export is ready.
    pub fn opened_for_writing(&mut self, handle: u32, path: &Path) {
        if self.exports.contains(path) {
            self.writing.insert(handle, path.to_path_buf());
        }
    }

    /// The app let go of `handle`.
    pub fn closed(&mut self, handle: u32) {
        if let Some(path) = self.writing.remove(&handle) {
            if !self.ready.contains(&path) {
                self.ready.push(path);
            }
        }
    }

    /// Export files written and let go of since last asked.
    pub fn take_ready_exports(&mut self) -> Vec<PathBuf> {
        std::mem::take(&mut self.ready)
    }

    /// The folder behind a token, if this run issued it.
    pub fn resolve_folder(&self, token: &str) -> Option<&Path> {
        self.folders_by_token.get(token).map(PathBuf::as_path)
    }

    /// The path behind a token, if this run issued it.
    ///
    /// A token from a previous run, an invented one, or one for a file the app
    /// was never offered all answer `None` -- there is no way to guess a path
    /// into this map.
    pub fn resolve(&self, token: &str) -> Option<&Path> {
        self.by_token.get(token).map(PathBuf::as_path)
    }

    /// The display name for a path: the file's own name and nothing else.
    ///
    /// An app showing "report.pdf" is useful; an app showing
    /// "/Users/someone/Documents/tax/2026/report.pdf" has learned the person's
    /// name, their folder habits, and what else is nearby.
    pub fn display_name(path: &Path) -> String {
        path.file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "file".to_string())
    }

    /// Everything this run is holding, files and folders together.
    ///
    /// One number because there is one budget: both kinds are handles a
    /// person clicked into existence, and both cost memory for the run.
    /// Keeping the two counts separate is what let the bound be exceeded
    /// (K-389).
    fn held(&self) -> usize {
        self.by_token.len() + self.folders_by_token.len()
    }

    /// How many files this run is holding, for tests and evidence.
    pub fn len(&self) -> usize {
        self.by_token.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_token.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_phone_save_is_ready_once_written_and_let_go_of_and_only_then() {
        let mut files = ChosenFiles::new();
        let saved = PathBuf::from("/tmp/krate-saves/1-0/Sketch.png");
        let other = PathBuf::from("/tmp/opened.txt");
        files.remember_export(saved.clone()).unwrap();
        files.remember(other.clone()).unwrap();
        files.opened_for_writing(7, &saved);
        files.opened_for_writing(8, &other);
        assert!(files.take_ready_exports().is_empty(), "still being written");
        files.closed(8);
        assert!(
            files.take_ready_exports().is_empty(),
            "an ordinary file is no save"
        );
        files.closed(7);
        assert_eq!(files.take_ready_exports(), vec![saved]);
        assert!(files.take_ready_exports().is_empty(), "handed over once");
    }

    #[test]
    fn a_token_resolves_only_for_the_run_that_issued_it() {
        let mut first = ChosenFiles::new();
        let token = first
            .remember(PathBuf::from("/tmp/report.pdf"))
            .expect("token");
        assert_eq!(first.resolve(&token), Some(Path::new("/tmp/report.pdf")));

        // A second run is a second store. The token from the first means
        // nothing in it, which is what stops an app saving a token and coming
        // back later for a file nobody offered again.
        let second = ChosenFiles::new();
        assert_eq!(second.resolve(&token), None);
    }

    #[test]
    fn an_invented_token_opens_nothing() {
        let mut files = ChosenFiles::new();
        files.remember(PathBuf::from("/tmp/a.txt")).expect("token");
        for guess in ["chosen-2", "chosen-0", "", "../etc/passwd", "chosen-1 "] {
            assert_eq!(
                files.resolve(guess),
                None,
                "{guess:?} should not resolve to a path"
            );
        }
    }

    #[test]
    fn a_token_is_never_reused_within_a_run() {
        // Two files chosen in one run must not collide, or picking a second
        // file would silently hand back the first.
        let mut files = ChosenFiles::new();
        let a = files.remember(PathBuf::from("/tmp/a.txt")).expect("a");
        let b = files.remember(PathBuf::from("/tmp/b.txt")).expect("b");
        assert_ne!(a, b);
        assert_eq!(files.resolve(&a), Some(Path::new("/tmp/a.txt")));
        assert_eq!(files.resolve(&b), Some(Path::new("/tmp/b.txt")));
    }

    #[test]
    fn a_token_carries_nothing_about_the_path() {
        // The token is what the app sees. If any of the path leaked into it,
        // the app would learn where the file lives without ever being told.
        let mut files = ChosenFiles::new();
        let token = files
            .remember(PathBuf::from(
                "/Users/someone/Documents/tax/2026/report.pdf",
            ))
            .expect("token");
        for secret in ["someone", "Documents", "tax", "2026", "report"] {
            assert!(
                !token.contains(secret),
                "the token leaked {secret:?}: {token}"
            );
        }
    }

    #[test]
    fn the_display_name_is_the_file_name_and_nothing_more() {
        assert_eq!(
            ChosenFiles::display_name(Path::new("/Users/someone/Documents/report.pdf")),
            "report.pdf"
        );
    }

    #[test]
    fn one_run_cannot_hold_unbounded_files() {
        let mut files = ChosenFiles::new();
        for i in 0..MAX_CHOSEN_FILES {
            assert!(
                files
                    .remember(PathBuf::from(format!("/tmp/{i}.txt")))
                    .is_some(),
                "file {i} should fit"
            );
        }
        assert_eq!(files.len(), MAX_CHOSEN_FILES);
        // A loop calling the picker stops here rather than growing forever.
        assert_eq!(files.remember(PathBuf::from("/tmp/extra.txt")), None);
    }

    /// Files and folders share ONE budget, in either order (K-389).
    ///
    /// The test above only ever adds files, so it passed while the mixed
    /// case did not hold: `remember` counted only files while
    /// `remember_folder` counted both, and filling with folders first then
    /// files gave 64 of each -- 128 handles against a bound of 64.
    ///
    /// Both orders, because checking one would pass with the bug still
    /// present on the other side.
    #[test]
    fn files_and_folders_share_one_budget_whichever_comes_first() {
        for folders_first in [true, false] {
            let mut held = ChosenFiles::new();
            let mut issued = 0;

            // Half the budget in one kind.
            let half = MAX_CHOSEN_FILES / 2;
            for i in 0..half {
                let got = if folders_first {
                    held.remember_folder(PathBuf::from(format!("/tmp/dir{i}")))
                } else {
                    held.remember(PathBuf::from(format!("/tmp/{i}.txt")))
                };
                assert!(got.is_some(), "the first half fits");
                issued += 1;
            }

            // Then the other kind until everything refuses.
            loop {
                let got = if folders_first {
                    held.remember(PathBuf::from("/tmp/extra.txt"))
                } else {
                    held.remember_folder(PathBuf::from("/tmp/extra"))
                };
                match got {
                    Some(_) => issued += 1,
                    None => break,
                }
                assert!(
                    issued <= MAX_CHOSEN_FILES,
                    "issued {issued} handles against a bound of {MAX_CHOSEN_FILES} \
                     (folders_first={folders_first})"
                );
            }

            assert_eq!(
                issued, MAX_CHOSEN_FILES,
                "the budget is spent exactly once (folders_first={folders_first})"
            );
            // And the other kind is refused too, rather than having its own.
            assert_eq!(held.remember(PathBuf::from("/tmp/more.txt")), None);
            assert_eq!(held.remember_folder(PathBuf::from("/tmp/more")), None);
        }
    }
}
