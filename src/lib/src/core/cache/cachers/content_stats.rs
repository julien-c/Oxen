//! Computes metadata we can extract from the entry files

use crate::constants::{CACHE_DIR, DIRS_DIR, HISTORY_DIR};
use crate::core::df::tabular;
use crate::core::index::{commit_metadata_db, CommitEntryReader};
use crate::error::OxenError;
use crate::model::{Commit, LocalRepository};
use crate::util;

use std::path::{Path, PathBuf};

pub fn dir_column_path(
    repo: &LocalRepository,
    commit: &Commit,
    dir: &Path,
    column: &str,
) -> PathBuf {
    util::fs::oxen_hidden_dir(&repo.path)
        .join(HISTORY_DIR)
        .join(&commit.id)
        .join(CACHE_DIR)
        .join(DIRS_DIR)
        .join(dir)
        .join(format!("{}.parquet", column))
}

pub fn compute(repo: &LocalRepository, commit: &Commit) -> Result<(), OxenError> {
    log::debug!("Running content_metadata");

    log::debug!("computing metadata {} -> {}", commit.id, commit.message);

    // Compute the metadata stats
    commit_metadata_db::index_commit(repo, commit)?;

    // Then for each directory, aggregate up the data_type and mime_type, and save as a dataframe file
    // that we can serve up.
    let reader = CommitEntryReader::new(repo, commit)?;
    let dirs = reader.list_dirs()?;
    log::debug!("Computing size of {} dirs", dirs.len());
    let columns = vec!["data_type", "mime_type"];
    for dir in dirs {
        for column in columns.iter() {
            log::debug!("Aggregating {column} for commit {commit:?}");
            let mut df = commit_metadata_db::aggregate_col(repo, commit, &dir, column)?;
            let path = dir_column_path(repo, commit, &dir, column);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            log::debug!("Writing cached df {} to {}", column, path.display());
            tabular::write_df(&mut df, &path)?;
        }
    }

    Ok(())
}