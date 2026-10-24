//! Read-only history inspections never block the UI or publish cancelled results.
use crate::{
    operations_workspace::{HistoryControl, HistoryRevision, RevisionEntry},
    BlameLine, FileDiff, GitRepo,
};
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc::{self, Receiver},
        Arc,
    },
};
pub enum Request {
    History {
        path: PathBuf,
        revision: String,
        follow: bool,
        all: bool,
        selection: Option<(usize, usize)>,
    },
    Blame {
        path: PathBuf,
        revision: String,
        ignore_whitespace: bool,
    },
    Compare {
        old: String,
        new: String,
        path: Option<PathBuf>,
    },
    Browse {
        revision: String,
        directory: PathBuf,
    },
}
pub enum Result {
    History(Vec<HistoryRevision>),
    Blame(Vec<BlameLine>),
    Compare(Vec<FileDiff>),
    Browse(Vec<RevisionEntry>),
}
pub struct Inspection {
    cancel: Arc<AtomicBool>,
    pub progress: Arc<AtomicUsize>,
    receiver: Receiver<std::result::Result<Result, String>>,
}
impl Drop for Inspection {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
    }
}
impl Inspection {
    pub fn start(root: PathBuf, request: Request) -> Self {
        let cancel = Arc::new(AtomicBool::new(false));
        let progress = Arc::new(AtomicUsize::new(0));
        let (sender, receiver) = mpsc::channel();
        let stopped = cancel.clone();
        let visited = progress.clone();
        std::thread::spawn(move || {
            let result = (|| -> anyhow::Result<Result> {
                let repo = GitRepo::discover(&root)
                    .ok_or_else(|| anyhow::anyhow!("Repository unavailable"))?;
                let result = match request {
                    Request::History {
                        path,
                        revision,
                        follow,
                        all,
                        selection,
                    } => {
                        if let Some(selection) = selection {
                            Result::History(
                                repo.selection_history(&path, &revision, selection, false)?
                                    .into_iter()
                                    .map(|commit| HistoryRevision {
                                        commit,
                                        path: path.clone(),
                                    })
                                    .collect(),
                            )
                        } else {
                            Result::History(repo.file_history_cancellable(
                                &path,
                                &revision,
                                follow,
                                1000,
                                all,
                                HistoryControl {
                                    cancel: &stopped,
                                    progress: &visited,
                                },
                            )?)
                        }
                    }
                    Request::Blame {
                        path,
                        revision,
                        ignore_whitespace,
                    } => Result::Blame(repo.blame_revision(
                        &path,
                        &revision,
                        ignore_whitespace,
                        None,
                    )?),
                    Request::Compare { old, new, path } => {
                        Result::Compare(repo.compare_revision_paths(&old, &new, path.as_deref())?)
                    }
                    Request::Browse {
                        revision,
                        directory,
                    } => Result::Browse(repo.revision_entries(&revision, &directory)?),
                };
                anyhow::ensure!(!stopped.load(Ordering::Acquire), "Inspection cancelled");
                Ok(result)
            })();
            let _ = sender.send(result.map_err(|error| error.to_string()));
        });
        Self {
            cancel,
            progress,
            receiver,
        }
    }
    pub fn poll(&self) -> Option<std::result::Result<Result, String>> {
        self.receiver.try_recv().ok()
    }
}
