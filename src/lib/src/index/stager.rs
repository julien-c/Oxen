use crate::constants;
use crate::db;
use crate::error::OxenError;
use crate::index::{
    path_db, CommitDirReader, CommitDirEntryReader, MergeConflictReader, Merger,
    StagedDirEntriesDB,
};
use crate::model::entry::staged_entry::StagedEntryType;
use crate::model::{
    CommitEntry, LocalRepository, MergeConflict, StagedData, StagedDirStats, StagedEntry,
    StagedEntryStatus,
};
use crate::util;

use filetime::FileTime;
use indicatif::ProgressBar;
use jwalk::WalkDirGeneric;
use rayon::prelude::*;
use rocksdb::{DBWithThreadMode, IteratorMode, MultiThreaded};
use std::fs;
use std::path::{Path, PathBuf};
use std::str;

pub const STAGED_DIR: &str = "staged";

#[derive(Debug, PartialEq, Eq, Clone)]
pub enum FileStatus {
    Added,
    Untracked,
    Modified,
    Removed,
    Conflict,
}

pub struct Stager {
    dir_db: DBWithThreadMode<MultiThreaded>,
    pub repository: LocalRepository,
    merger: Option<Merger>,
}

impl Stager {
    pub fn dirs_db_path(path: &Path) -> PathBuf {
        util::fs::oxen_hidden_dir(path).join(Path::new(STAGED_DIR)).join("dirs/")
    }

    pub fn new(repository: &LocalRepository) -> Result<Stager, OxenError> {
        let db_path = Stager::dirs_db_path(&repository.path);
        log::debug!("Stager new dir db_path {:?}", db_path);
        if !db_path.exists() {
            std::fs::create_dir_all(&db_path)?;
        }
        let opts = db::opts::default();
        Ok(Stager {
            dir_db: DBWithThreadMode::open(&opts, &db_path)?,
            repository: repository.clone(),
            merger: None,
        })
    }

    pub fn new_with_merge(repository: &LocalRepository) -> Result<Stager, OxenError> {
        let db_path = Stager::dirs_db_path(&repository.path);
        log::debug!("Stager new_with_merge dir db_path {:?}", db_path);
        if !db_path.exists() {
            std::fs::create_dir_all(&db_path)?;
        }
        let opts = db::opts::default();
        Ok(Stager {
            dir_db: DBWithThreadMode::open(&opts, &db_path)?,
            repository: repository.clone(),
            merger: Some(Merger::new(&repository.clone())?),
        })
    }

    pub fn add(&self, path: &Path, commit_reader: &CommitDirReader) -> Result<(), OxenError> {
        if path
            .to_str()
            .unwrap()
            .to_string()
            .contains(constants::OXEN_HIDDEN_DIR)
        {
            return Ok(());
        }

        log::debug!("stager.add({:?})", path);

        if path == Path::new(".") {
            for entry in (std::fs::read_dir(path)?).flatten() {
                let path = entry.path();
                let entry_path = self.repository.path.join(&path);
                self.add(&entry_path, commit_reader)?;
            }
            log::debug!("ADD CURRENT DIR: {:?}", path);
            return Ok(());
        }

        // If it doesn't exist on disk, it might have been removed, and we can't tell if it is a file or dir
        // so we have to check if it is committed, and what the backup version is
        if !path.exists() {
            let relative_path = util::fs::path_relative_to_dir(path, &self.repository.path)?;
            log::debug!(
                "Stager.add() !path.exists() checking relative path: {:?}",
                relative_path
            );
            // Since entries that are committed are only files.. we will have to have different logic for dirs
            if let Ok(Some(value)) = commit_reader.get_entry(&relative_path) {
                self.add_removed_file(&relative_path, &value)?;
                return Ok(());
            }

            let files_in_dir = commit_reader.list_files_from_dir(&relative_path);
            if !files_in_dir.is_empty() {
                for entry in files_in_dir.iter() {
                    self.add_removed_file(&entry.path, entry)?;
                }

                log::debug!(
                    "Stager.add() !path.exists() !files_in_dir.is_empty() {:?}",
                    path
                );
                return Ok(());
            }
        }

        log::debug!("Stager.add() is_dir? {} path: {:?}", path.is_dir(), path);
        if path.is_dir() {
            match self.add_dir(path, commit_reader) {
                Ok(_) => Ok(()),
                Err(err) => Err(err),
            }
        } else {
            match self.add_file(path, commit_reader) {
                Ok(_) => Ok(()),
                Err(err) => Err(err),
            }
        }
    }

    pub fn status(&self, entry_reader: &CommitDirReader) -> Result<StagedData, OxenError> {
        self.compute_staged_data(&self.repository.path, entry_reader)
    }

    fn list_merge_conflicts(&self) -> Result<Vec<MergeConflict>, OxenError> {
        let merger = MergeConflictReader::new(&self.repository)?;
        merger.list_conflicts()
    }

    fn compute_staged_data(
        &self,
        dir: &Path,
        entry_reader: &CommitDirReader,
    ) -> Result<StagedData, OxenError> {
        log::debug!("compute_staged_data listing eligable {:?}", dir);
        let mut status = StagedData::empty();

        // Start with candidate dirs from committed and added, not all the dirs
        let added_dirs = self.list_added_dirs()?;
        log::debug!("compute_staged_data Got added dirs: {}", added_dirs.len());
        for dir in added_dirs {
            log::debug!("compute_staged_data considering added dir {:?}", dir);
            let stats = self.compute_staged_dir_stats(&dir)?;
            status.added_dirs.insert(stats);

            self.check_status_for_all_files_in_dir(&dir, &mut status);
        }


        let committed_dirs = entry_reader.list_committed_dirs()?;
        for dir in committed_dirs {
            self.check_status_for_all_files_in_dir(&dir, &mut status);
        }

        for path in std::fs::read_dir(dir)? {
            let path = path?.path();
            log::debug!("compute_staged_data considering path {:?}", path);

            if path.is_dir() {
                if !self.has_staged_dir(&path) {
                    status.untracked_dirs.push((path, 0));
                }
            } else {
                if let Ok(Some(entry)) = self.get_entry(&path) {
                    let path = util::fs::path_relative_to_dir(&path, &self.repository.path).unwrap();
                    status.added_files.push((path, entry))
                }
            }
        }

        status.merge_conflicts = self.list_merge_conflicts()?;

        Ok(status)
    }

    fn check_status_for_all_files_in_dir(&self, dir: &Path, status: &mut StagedData) {
        log::debug!("check_status_for_all_files_in_dir: {:?}", dir);
        let repository = self.repository.to_owned();
        for dir_entry_result in WalkDirGeneric::<((), Option<FileStatus>)>::new(&dir)
            .skip_hidden(true)
            .process_read_dir(move |_, parent, _, dir_entry_results| {
                log::debug!("check_status_for_all_files_in_dir process_dir {:?}", parent);
                let staged_dir_db = StagedDirEntriesDB::new(&repository, parent).unwrap();
                let commit_dir_db = CommitDirEntryReader::new(&repository, parent);

                dir_entry_results.iter_mut().for_each(|dir_entry_result| {
                    if let Ok(dir_entry) = dir_entry_result {
                        if !dir_entry.file_type.is_dir() {
                            // Entry is file
                            let path = dir_entry.path();
                            let path =
                                util::fs::path_relative_to_dir(&path, &repository.path).unwrap();

                            if staged_dir_db.has_entry(&path) {
                                dir_entry.client_state = Some(FileStatus::Added);
                                return;
                            } else {
                                // Not in the staged DB
                                // check if it is in the HEAD commit
                                if let Ok(commit_dir_db) = &commit_dir_db {
                                    if let Ok(Some(commit_entry)) = commit_dir_db.get_entry(&path) {
                                        // Get last modified time
                                        let metadata = fs::metadata(&path).unwrap();
                                        let mtime = FileTime::from_last_modification_time(&metadata);

                                        // log::debug!("comparing timestamps: {} to {}", old_entry.last_modified_nanoseconds, mtime.nanoseconds());

                                        if commit_entry.has_different_modification_time(&mtime) {
                                            // log::debug!("stager::list_modified_files modification times are different! {:?}", relative_path);

                                            // Then check the hashes, because the data might not be different, timestamp is just an optimization
                                            let hash = util::hasher::hash_file_contents(&path).unwrap();
                                            if hash != commit_entry.hash {
                                                dir_entry.client_state = Some(FileStatus::Modified)
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                })
            })
        {
            match dir_entry_result {
                Ok(dir_entry) => {
                    if let Some(file_type) = &dir_entry.client_state {
                        match file_type {
                            FileStatus::Added => {
                                let path = dir_entry.path();
                                if let Ok(Some(entry)) = self.get_entry(&path) {
                                    let path = util::fs::path_relative_to_dir(&path, &self.repository.path).unwrap();
                                    status.added_files.push((path, entry))
                                }
                            }
                            FileStatus::Untracked => {
                                status.untracked_files.push(dir_entry.path());
                            }
                            FileStatus::Modified => {
                                status.modified_files.push(dir_entry.path());
                            }
                            FileStatus::Removed => {
                                // files.push(dir_entry.path());
                            }
                            FileStatus::Conflict => {
                                // files.push(dir_entry.path());
                            }
                        }
                    }
                }
                Err(error) => {
                    println!("Read dir_entry error: {}", error);
                }
            }
        }
        log::debug!(
            "compute_staged_data untracked files: {}",
            status.untracked_files.len()
        );
        log::debug!(
            "compute_staged_data untracked dirs: {}",
            status.untracked_dirs.len()
        );
        log::debug!(
            "compute_staged_data added files len: {}",
            status.added_files.len()
        );
        log::debug!(
            "compute_staged_data added dirs len: {}",
            status.added_dirs.len()
        );
    }

    fn add_removed_file(&self, path: &Path, entry: &CommitEntry) -> Result<StagedEntry, OxenError> {
        if let (Some(parent), Some(filename)) = (path.parent(), path.file_name()) {
            let staged_dir = StagedDirEntriesDB::new(&self.repository, &parent)?;
            staged_dir.add_removed_file(filename, entry)
        } else {
            Err(OxenError::file_has_no_parent(path))
        }
    }

    pub fn add_dir(&self, dir: &Path, entry_reader: &CommitDirReader) -> Result<(), OxenError> {
        if !dir.exists() || !dir.is_dir() {
            let err = format!("Cannot stage non-existant dir: {:?}", dir);
            return Err(OxenError::basic_str(&err));
        }

        // Add all untracked files and modified files
        let mut status = StagedData::empty();
        self.check_status_for_all_files_in_dir(dir, &mut status);
        let mut paths = status.untracked_files;
        let mut modified_paths = status.modified_files;
        paths.append(&mut modified_paths);

        log::debug!("Stager.add_dir {:?} -> {}", dir, paths.len());

        let short_path = util::fs::path_relative_to_dir(dir, &self.repository.path)?;
        println!("Adding files in directory: {:?}", short_path);
        let size: u64 = unsafe { std::mem::transmute(paths.len()) };
        let bar = ProgressBar::new(size);
        paths.par_iter().for_each(|path| {
            let full_path = self.repository.path.join(path);
            match self.add_file(&full_path, entry_reader) {
                Ok(_) => {
                    // all good
                }
                Err(err) => {
                    log::error!("Could not add file: {:?}\nErr: {}", path, err);
                }
            }
            bar.inc(1);
        });

        bar.finish();

        Ok(())
    }

    pub fn has_staged_dir<P: AsRef<Path>>(&self, dir: P) -> bool {
        path_db::has_entry(&self.dir_db, dir)
    }

    pub fn has_entry<P: AsRef<Path>>(&self, path: P) -> bool {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            if let Ok(staged_dir) = StagedDirEntriesDB::new(&self.repository, &parent) {
                return staged_dir.has_entry(path);
            }
        }
        false
    }

    pub fn get_entry(&self, path: &Path) -> Result<Option<StagedEntry>, OxenError> {
        let relative = util::fs::path_relative_to_dir(path, &self.repository.path)?;
        if let Some(parent) = relative.parent() {
            log::debug!("get_entry got parent for path {:?} -> {:?}", path, parent);
            log::debug!("get_entry relative {:?}", relative);
            let staged_db = StagedDirEntriesDB::new(&self.repository, &parent)?;
            return staged_db.get_entry(relative);
        } else {
            log::debug!("get_entry no parent for path: {:?}", path);
        }
        Ok(None)
    }

    pub fn add_file(
        &self,
        path: &Path,
        entry_reader: &CommitDirReader,
    ) -> Result<PathBuf, OxenError> {
        self.add_staged_entry(path, entry_reader, StagedEntryType::Regular)
    }

    pub fn add_tabular_file(
        &self,
        path: &Path,
        entry_reader: &CommitDirReader,
    ) -> Result<PathBuf, OxenError> {
        self.add_staged_entry(path, entry_reader, StagedEntryType::Tabular)
    }

    fn add_staged_entry(
        &self,
        path: &Path,
        entry_reader: &CommitDirReader,
        entry_type: StagedEntryType,
    ) -> Result<PathBuf, OxenError> {
        // We should have normalized to path past repo at this point
        log::debug!("Add file: {:?} to {:?}", path, self.repository.path);
        if !path.exists() {
            return Err(OxenError::file_does_not_exist(path));
        }

        // compute the hash to know if it has changed
        let hash = util::hasher::hash_file_contents(path)?;

        // Key is the filename relative to the repository
        // if repository: /Users/username/Datasets/MyRepo
        //   /Users/username/Datasets/MyRepo/train -> train
        //   /Users/username/Datasets/MyRepo/annotations/train.txt -> annotations/train.txt
        let path = util::fs::path_relative_to_dir(path, &self.repository.path)?;

        let mut staged_entry = StagedEntry {
            hash: hash.to_owned(),
            status: StagedEntryStatus::Added,
            entry_type: entry_type,
        };

        // Check if it is a merge conflict, then we can add it
        if let Some(merger) = &self.merger {
            if merger.has_file(&path)? {
                log::debug!("add_file merger has file! {:?}", path);
                self.add_staged_entry_to_db(&path, &staged_entry)?;
                merger.remove_conflict_path(&path)?;
                return Ok(path);
            }
        }

        // Check if file has changed on disk
        if let Ok(Some(entry)) = entry_reader.get_entry(&path) {
            if entry.hash == hash {
                // file has not changed, don't add it
                log::debug!("add_file do not add file, it hasn't changed: {:?}", path);
                return Ok(path);
            } else {
                // Hash doesn't match, mark it as modified
                staged_entry.status = StagedEntryStatus::Modified;
            }
        }

        log::debug!("add_staged_entry_to_db {:?}", staged_entry);
        self.add_staged_entry_to_db(&path, &staged_entry)?;

        Ok(path)
    }

    fn add_staged_entry_to_db(
        &self,
        path: &Path,
        staged_entry: &StagedEntry,
    ) -> Result<(), OxenError> {
        let relative = util::fs::path_relative_to_dir(&path, &self.repository.path)?;
        if let (Some(parent), Some(filename)) = (relative.parent(), relative.file_name()) {
            log::debug!("add_staged_entry_to_db adding file {:?} to parent {:?}", filename, parent);

            if parent != self.repository.path {
                path_db::add_to_db(&self.dir_db, parent, &0)?;
            }

            let staged_dir = StagedDirEntriesDB::new(&self.repository, &parent)?;
            staged_dir.add_staged_entry_to_db(filename, staged_entry)
        } else {
            Err(OxenError::file_has_no_parent(path))
        }
    }

    fn list_added_files_in_dir(&self, dir: &Path) -> Result<Vec<PathBuf>, OxenError> {
        if let Some(parent) = dir.parent() {
            let staged_dir = StagedDirEntriesDB::new(&self.repository, &parent)?;
            staged_dir.list_added_paths()
        } else {
            Err(OxenError::file_has_no_parent(dir))
        }
    }

    pub fn list_added_dirs(&self) -> Result<Vec<PathBuf>, OxenError> {
        path_db::list_paths(&self.dir_db, Path::new(""))
    }

    pub fn compute_staged_dir_stats(&self, path: &Path) -> Result<StagedDirStats, OxenError> {
        let relative_path = util::fs::path_relative_to_dir(&path, &self.repository.path)?;
        let mut stats = StagedDirStats {
            path: relative_path,
            num_files_staged: 0,
            total_files: 0,
        };

        // Only consider directories
        if !path.is_dir() {
            return Ok(stats);
        }

        // Count in db from relative path
        let num_files_staged = self.list_added_files_in_dir(&path)?.len();

        // Make sure we have some files added
        if num_files_staged == 0 {
            return Ok(stats);
        }

        // Count in fs from full path
        stats.total_files = util::fs::rcount_files_in_dir(&path);
        stats.num_files_staged = num_files_staged;

        Ok(stats)
    }

    pub fn list_removed_files(
        &self,
        entry_reader: &CommitDirReader,
    ) -> Result<Vec<PathBuf>, OxenError> {
        // TODO: We are looping multiple times to check whether file is added,modified,or removed, etc
        //       We should do this loop once, and check each thing
        let mut paths: Vec<PathBuf> = vec![];
        for short_path in entry_reader.list_files()? {
            let path = self.repository.path.join(&short_path);
            if !path.exists() && !self.has_entry(&short_path) {
                paths.push(short_path);
            }
        }
        Ok(paths)
    }

    pub fn list_modified_files(
        &self,
        entry_reader: &CommitDirReader,
    ) -> Result<Vec<PathBuf>, OxenError> {
        // TODO: We are looping multiple times to check whether file is added,modified,or removed, etc
        //       We should do this loop once, and check each thing
        let dir_entries = util::fs::rlist_files_in_dir(&self.repository.path);

        let mut paths: Vec<PathBuf> = vec![];
        for local_path in dir_entries.iter() {
            if local_path.is_file() {
                // Return relative path with respect to the repo
                let relative_path =
                    util::fs::path_relative_to_dir(local_path, &self.repository.path)?;

                // log::debug!("stager::list_modified_files considering path {:?}", relative_path);

                if self.has_entry(&relative_path) {
                    // log::debug!("stager::list_modified_files already added path {:?}", relative_path);
                    continue;
                }

                // Check if we have the entry in the head commit
                if let Ok(Some(old_entry)) = entry_reader.get_entry(&relative_path) {
                    // Get last modified time
                    let metadata = fs::metadata(local_path).unwrap();
                    let mtime = FileTime::from_last_modification_time(&metadata);

                    // log::debug!("comparing timestamps: {} to {}", old_entry.last_modified_nanoseconds, mtime.nanoseconds());

                    if old_entry.has_different_modification_time(&mtime) {
                        // log::debug!("stager::list_modified_files modification times are different! {:?}", relative_path);

                        // Then check the hashes, because the data might not be different, timestamp is just an optimization
                        let hash = util::hasher::hash_file_contents(local_path)?;
                        if hash != old_entry.hash {
                            paths.push(relative_path);
                        }
                    }
                } else {
                    // log::debug!("stager::list_modified_files we don't have file in head commit {:?}", relative_path);
                }
            }
        }

        Ok(paths)
    }

    pub fn list_untracked_files(
        &self,
        entry_reader: &CommitDirReader,
    ) -> Result<Vec<PathBuf>, OxenError> {
        let dir_entries = std::fs::read_dir(&self.repository.path)?;
        // println!("Listing untracked files from {:?}", dir_entries);
        let num_in_head = entry_reader.num_entries()?;
        log::debug!(
            "stager::list_untracked_files head has {} files",
            num_in_head
        );

        let mut paths: Vec<PathBuf> = vec![];
        for entry in dir_entries {
            let local_path = entry?.path();
            if local_path.is_file() {
                // Return relative path with respect to the repo
                let relative_path =
                    util::fs::path_relative_to_dir(&local_path, &self.repository.path)?;
                log::debug!(
                    "stager::list_untracked_files considering path {:?}",
                    relative_path
                );

                // File is committed in HEAD
                if entry_reader.has_file(&relative_path) {
                    continue;
                }

                // File is staged
                if !self.has_entry(&relative_path) {
                    paths.push(relative_path);
                }
            }
        }

        Ok(paths)
    }

    pub fn list_untracked_directories(
        &self,
        entry_writer: &CommitDirReader,
    ) -> Result<Vec<(PathBuf, usize)>, OxenError> {
        log::debug!("list_untracked_directories {:?}", self.repository.path);
        let dir_entries: Vec<PathBuf> = std::fs::read_dir(&self.repository.path)?
            .filter_map(|entry| entry.ok().and_then(|e| Some(e.path())))
            .collect();
        log::debug!(
            "list_untracked_directories considering {} entries",
            dir_entries.len()
        );

        let mut paths: Vec<(PathBuf, usize)> = vec![];
        panic!("TODO: redo");

        // for path in dir_entries {
        //     // log::debug!("list_untracked_directories considering {:?}", path);
        //     if path.is_dir() {
        //         let relative_path = util::fs::path_relative_to_dir(&path, &self.repository.path)?;
        //         // log::debug!("list_untracked_directories relative {:?}", relative_path);

        //         if entry_writer.has_file(&relative_path) {
        //             continue;
        //         }

        //         if let Some(path_str) = relative_path.to_str() {
        //             if path_str.contains(constants::OXEN_HIDDEN_DIR) {
        //                 continue;
        //             }

        //             let bytes = path_str.as_bytes();
        //             match self.db.get(bytes) {
        //                 Ok(Some(_value)) => {
        //                     // already added
        //                     // println!("got value: {:?}", value);
        //                 }
        //                 Ok(None) => {
        //                     // did not get val
        //                     // println!("list_untracked_directories get file count in: {:?}", path);

        //                     // TODO: Speed this up
        //                     let count = self.count_untracked_files_in_dir(&path, entry_writer);
        //                     if count > 0 {
        //                         paths.push((relative_path, count));
        //                     }
        //                 }
        //                 Err(err) => {
        //                     eprintln!("{}", err);
        //                 }
        //             }
        //         }
        //     }
        // }

        Ok(paths)
    }

    pub fn unstage(&self) -> Result<(), OxenError> {
        for dir in self.list_added_dirs()? {
            let staged_dir = StagedDirEntriesDB::new(&self.repository, &dir)?;
            staged_dir.unstage()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::error::OxenError;
    use crate::index::{CommitDirReader, CommitReader, CommitWriter, Stager};
    use crate::model::{StagedDirStats, StagedEntryStatus};
    use crate::test;
    use crate::util;

    use std::path::{Path, PathBuf};

    #[test]
    fn test_1_stager_add_file() -> Result<(), OxenError> {
        test::run_empty_stager_test(|stager, repo| {
            // Create entry_reader with no commits

            let commit_reader = CommitReader::new(&repo)?;
            let commit = commit_reader.head_commit()?;
            let entry_reader = CommitDirReader::new(&stager.repository, &commit)?;

            // Write a file to disk
            let repo_path = &stager.repository.path;
            let hello_file = test::add_txt_file_to_dir(repo_path, "Hello World")?;

            // Add the file
            let path = stager.add_file(&hello_file, &entry_reader)?;

            // Make sure we saved the relative path
            let relative_path = util::fs::path_relative_to_dir(&hello_file, repo_path)?;
            assert_eq!(path, relative_path);

            Ok(())
        })
    }

    #[test]
    fn test_stager_unstage() -> Result<(), OxenError> {
        test::run_empty_stager_test(|stager, repo| {
            // Create entry_reader with no commits
            let commit_reader = CommitReader::new(&repo)?;
            let commit = commit_reader.head_commit()?;
            let entry_reader = CommitDirReader::new(&stager.repository, &commit)?;

            let repo_path = &stager.repository.path;
            let hello_file = test::add_txt_file_to_dir(repo_path, "Hello World")?;

            let sub_dir = repo_path.join("training_data");
            std::fs::create_dir_all(&sub_dir)?;
            let _ = test::add_txt_file_to_dir(&sub_dir, "Hello 1")?;
            let _ = test::add_txt_file_to_dir(&sub_dir, "Hello 2")?;

            // Add a file and a directory
            stager.add_file(&hello_file, &entry_reader)?;
            stager.add_dir(&sub_dir, &entry_reader)?;

            // Make sure the counts start properly
            let status = stager.status(&entry_reader)?;
            assert_eq!(status.added_files.len(), 3);
            assert!(false);
            // let dirs = stager.list_added_directories()?;
            // assert_eq!(dirs.len(), 1);

            // Unstage
            stager.unstage()?;

            // There should no longer be any added files
            let status = stager.status(&entry_reader)?;
            assert_eq!(status.added_files.len(), 0);
            assert!(false);
            // let dirs = stager.list_added_directories()?;
            // assert_eq!(dirs.len(), 0);

            Ok(())
        })
    }

    #[test]
    fn test_add_twice_only_adds_once() -> Result<(), OxenError> {
        test::run_empty_stager_test(|stager, _repo| {
            // Create entry_reader with no commits
            let entry_reader = CommitDirReader::new_from_head(&stager.repository)?;

            // Make sure we have a valid file
            let repo_path = &stager.repository.path;
            let hello_file = test::add_txt_file_to_dir(repo_path, "Hello World")?;

            // Add it twice
            stager.add_file(&hello_file, &entry_reader)?;
            stager.add_file(&hello_file, &entry_reader)?;

            // Make sure we still only have it once
            let status = stager.status(&entry_reader)?;
            assert_eq!(status.added_files.len(), 1);

            Ok(())
        })
    }

    #[test]
    fn test_cannot_add_if_not_different_from_commit() -> Result<(), OxenError> {
        test::run_empty_stager_test(|stager, repo| {
            // Create entry_reader with no commits
            let entry_reader = CommitDirReader::new_from_head(&stager.repository)?;

            // Make sure we have a valid file
            let repo_path = &stager.repository.path;
            let hello_file = test::add_txt_file_to_dir(repo_path, "Hello World")?;

            // Add it
            stager.add_file(&hello_file, &entry_reader)?;

            // Commit it
            let commit_writer = CommitWriter::new(&repo)?;
            let status = stager.status(&entry_reader)?;
            let commit = commit_writer.commit(&status, "Add Hello World")?;
            stager.unstage()?;

            // try to add it again
            let entry_reader = CommitDirReader::new(&repo, &commit)?;
            stager.add_file(&hello_file, &entry_reader)?;

            // make sure we don't have it added again, because the hash hadn't changed since last commit
            let status = stager.status(&entry_reader)?;
            assert_eq!(status.added_files.len(), 0);

            Ok(())
        })
    }

    #[test]
    fn test_add_non_existant_file() -> Result<(), OxenError> {
        test::run_empty_stager_test(|stager, _repo| {
            // Create entry_reader with no commits
            let entry_reader = CommitDirReader::new_from_head(&stager.repository)?;

            let hello_file = PathBuf::from("non-existant.txt");
            if stager.add_file(&hello_file, &entry_reader).is_ok() {
                // we don't want to be able to add this file
                panic!("test_add_non_existant_file() Cannot stage non-existant file")
            }

            Ok(())
        })
    }

    #[test]
    fn test_add_file() -> Result<(), OxenError> {
        test::run_empty_stager_test(|stager, _repo| {
            // Create entry_reader with no commits
            let entry_reader = CommitDirReader::new_from_head(&stager.repository)?;

            let hello_file = test::add_txt_file_to_dir(&stager.repository.path, "Hello 1")?;
            assert!(stager.add_file(&hello_file, &entry_reader).is_ok());

            let status = stager.status(&entry_reader)?;
            assert_eq!(status.added_files.len(), 1);

            Ok(())
        })
    }

    #[test]
    fn test_single_add_file_in_dir() -> Result<(), OxenError> {
        test::run_empty_stager_test(|stager, _repo| {
            // Create entry_reader with no commits
            let entry_reader = CommitDirReader::new_from_head(&stager.repository)?;

            // Write two files to directories
            let repo_path = &stager.repository.path;
            let sub_dir = repo_path.join("training_data").join("deeper");
            std::fs::create_dir_all(&sub_dir)?;
            let file = test::add_txt_file_to_dir(&sub_dir, "Hello 1")?;

            assert!(stager.add_file(&file, &entry_reader).is_ok());

            let status = stager.status(&entry_reader)?;
            assert_eq!(status.added_files.len(), 1);
            
            Ok(())
        })
    }

    #[test]
    fn test_add_directory() -> Result<(), OxenError> {
        test::run_empty_stager_test(|stager, _repo| {
            // Create entry_reader with no commits
            let entry_reader = CommitDirReader::new_from_head(&stager.repository)?;

            // Write two files to directories
            let repo_path = &stager.repository.path;
            let sub_dir = repo_path.join("training_data");
            std::fs::create_dir_all(&sub_dir)?;
            let _ = test::add_txt_file_to_dir(&sub_dir, "Hello 1")?;
            let _ = test::add_txt_file_to_dir(&sub_dir, "Hello 2")?;

            stager.add_dir(&sub_dir, &entry_reader)?;

            let status = stager.status(&entry_reader)?;
            assert_eq!(status.added_files.len(), 2);

            Ok(())
        })
    }

    #[test]
    fn test_stager_get_entry() -> Result<(), OxenError> {
        test::run_empty_stager_test(|stager, _repo| {
            // Create entry_reader with no commits
            let entry_reader = CommitDirReader::new_from_head(&stager.repository)?;

            let repo_path = &stager.repository.path;
            let hello_file = test::add_txt_file_to_dir(repo_path, "Hello World")?;
            let relative_path = util::fs::path_relative_to_dir(&hello_file, repo_path)?;

            // Stage file
            stager.add_file(&hello_file, &entry_reader)?;

            // we should be able to fetch this entry json
            let entry = stager.get_entry(&relative_path).unwrap().unwrap();
            assert!(!entry.hash.is_empty());
            assert_eq!(entry.status, StagedEntryStatus::Added);

            Ok(())
        })
    }

    #[test]
    fn test_stager_list_files() -> Result<(), OxenError> {
        test::run_empty_stager_test(|stager, _repo| {
            // Create entry_reader with no commits
            let entry_reader = CommitDirReader::new_from_head(&stager.repository)?;

            let repo_path = &stager.repository.path;
            let hello_file = test::add_txt_file_to_dir(repo_path, "Hello World")?;
            let relative_path = util::fs::path_relative_to_dir(&hello_file, repo_path)?;

            // Stage file
            stager.add_file(&hello_file, &entry_reader)?;

            // List files
            let status = stager.status(&entry_reader)?;
            let files = status.added_files;
            assert_eq!(files.len(), 1);

            assert_eq!(files[0].0, relative_path);

            Ok(())
        })
    }

    #[test]
    fn test_stager_add_file_in_sub_dir() -> Result<(), OxenError> {
        test::run_empty_stager_test(|stager, _repo| {
            // Create entry_reader with no commits
            let entry_reader = CommitDirReader::new_from_head(&stager.repository)?;

            // Write two files to a sub directory
            let repo_path = &stager.repository.path;
            let sub_dir = repo_path.join("training_data");
            std::fs::create_dir_all(&sub_dir)?;

            let _ = test::add_txt_file_to_dir(&sub_dir, "Hello 1")?;
            let sub_file = test::add_txt_file_to_dir(&sub_dir, "Hello 2")?;

            stager.add_file(&sub_file, &entry_reader)?;

            // List files
            let status = stager.status(&entry_reader)?;
            let files = status.added_files;

            // There is one file
            assert_eq!(files.len(), 1);
            let relative_path = util::fs::path_relative_to_dir(&sub_file, repo_path)?;
            assert_eq!(files[0].0, relative_path);

            Ok(())
        })
    }

    #[test]
    fn test_stager_add_file_in_sub_dir_updates_untracked_count() -> Result<(), OxenError> {
        test::run_empty_stager_test(|stager, _repo| {
            // Create entry_reader with no commits
            let entry_reader = CommitDirReader::new_from_head(&stager.repository)?;

            // Write two files to a sub directory
            let repo_path = &stager.repository.path;
            let sub_dir = repo_path.join("training_data");
            std::fs::create_dir_all(&sub_dir)?;

            let _ = test::add_txt_file_to_dir(&sub_dir, "Hello 1")?;
            let _ = test::add_txt_file_to_dir(&sub_dir, "Hello 2")?;
            let sub_file = test::add_txt_file_to_dir(&sub_dir, "Hello 3")?;

            let dirs = stager.list_untracked_directories(&entry_reader)?;
            // There is one directory
            assert_eq!(dirs.len(), 1);
            let relative_path = util::fs::path_relative_to_dir(&sub_dir, repo_path)?;
            assert_eq!(dirs[0].0, relative_path);

            // With three untracked files
            assert_eq!(dirs[0].1, 3);

            // Then we add one file
            stager.add_file(&sub_file, &entry_reader)?;

            // There are still two untracked files in the dir
            let dirs = stager.list_untracked_directories(&entry_reader)?;
            assert_eq!(dirs.len(), 1);
            assert_eq!(dirs[0].0, relative_path);

            // With two files
            assert_eq!(dirs[0].1, 2);

            Ok(())
        })
    }

    #[test]
    fn test_stager_add_all_files_in_sub_dir() -> Result<(), OxenError> {
        test::run_empty_stager_test(|stager, _repo| {
            // Create entry_reader with no commits
            let entry_reader = CommitDirReader::new_from_head(&stager.repository)?;

            // Write two files to a sub directory
            let repo_path = &stager.repository.path;
            let training_data_dir = PathBuf::from("training_data");
            let sub_dir = repo_path.join(&training_data_dir);
            std::fs::create_dir_all(&sub_dir)?;

            let sub_file_1 = test::add_txt_file_to_dir(&sub_dir, "Hello 1")?;
            let sub_file_2 = test::add_txt_file_to_dir(&sub_dir, "Hello 2")?;
            let sub_file_3 = test::add_txt_file_to_dir(&sub_dir, "Hello 3")?;

            let dirs = stager.list_untracked_directories(&entry_reader)?;

            // There is one directory
            assert_eq!(dirs.len(), 1);
            // With three untracked files
            assert_eq!(dirs[0].1, 3);

            // Then we add all three
            stager.add_file(&sub_file_1, &entry_reader)?;
            stager.add_file(&sub_file_2, &entry_reader)?;
            stager.add_file(&sub_file_3, &entry_reader)?;

            // There now there are no untracked directories
            let untracked_dirs = stager.list_untracked_directories(&entry_reader)?;
            assert_eq!(untracked_dirs.len(), 0);

            // And there is one tracked directory
            assert!(false);
            // let added_dirs = stager.list_added_directories()?;
            // assert_eq!(added_dirs.len(), 1);
            // let added_dir = added_dirs
            //     .get(&StagedDirStats::from_path(training_data_dir))
            //     .unwrap();
            // assert_eq!(added_dir.num_files_staged, 3);
            // assert_eq!(added_dir.total_files, 3);

            Ok(())
        })
    }

    #[test]
    fn test_stager_list_directories() -> Result<(), OxenError> {
        test::run_empty_stager_test(|stager, _repo| {
            // Create entry_reader with no commits
            let entry_reader = CommitDirReader::new_from_head(&stager.repository)?;

            // Write two files to a sub directory
            let repo_path = &stager.repository.path;
            let training_data_dir = PathBuf::from("training_data");
            let sub_dir = repo_path.join(&training_data_dir);
            std::fs::create_dir_all(&sub_dir)?;

            let _ = test::add_txt_file_to_dir(&sub_dir, "Hello 1")?;
            let _ = test::add_txt_file_to_dir(&sub_dir, "Hello 2")?;

            stager.add_dir(&sub_dir, &entry_reader)?;

            // List files
            assert!(false);

            // let dirs = stager.list_added_directories()?;

            // // There is one directory
            // assert_eq!(dirs.len(), 1);
            // let added_dir = dirs
            //     .get(&StagedDirStats::from_path(&training_data_dir))
            //     .unwrap();
            // assert_eq!(added_dir.path, training_data_dir);

            // // With two files
            // assert_eq!(added_dir.num_files_staged, 2);

            Ok(())
        })
    }

    #[test]
    fn test_stager_list_untracked_files() -> Result<(), OxenError> {
        test::run_empty_stager_test(|stager, _repo| {
            // Create entry_reader with no commits
            let entry_reader = CommitDirReader::new_from_head(&stager.repository)?;

            let repo_path = &stager.repository.path;
            let hello_file = test::add_txt_file_to_dir(repo_path, "Hello 1")?;

            // Do not add...

            // List files
            let files = stager.list_untracked_files(&entry_reader)?;
            assert_eq!(files.len(), 1);
            let relative_path = util::fs::path_relative_to_dir(&hello_file, repo_path)?;
            assert_eq!(files[0], relative_path);

            Ok(())
        })
    }

    #[test]
    fn test_stager_list_modified_files() -> Result<(), OxenError> {
        test::run_empty_stager_test(|stager, repo| {
            // Create entry_reader with no commits
            let entry_reader = CommitDirReader::new_from_head(&stager.repository)?;

            let repo_path = &stager.repository.path;
            let hello_file = test::add_txt_file_to_dir(repo_path, "Hello 1")?;

            // add the file
            stager.add_file(&hello_file, &entry_reader)?;

            // commit the file
            let status = stager.status(&entry_reader)?;
            let commit_writer = CommitWriter::new(&repo)?;
            let commit = commit_writer.commit(&status, "added hello 1")?;
            stager.unstage()?;

            let mod_files = stager.list_modified_files(&entry_reader)?;
            assert_eq!(mod_files.len(), 0);

            // modify the file
            let hello_file = test::modify_txt_file(hello_file, "Hello 2")?;

            // List files
            let entry_reader = CommitDirReader::new(&stager.repository, &commit)?;
            let mod_files = stager.list_modified_files(&entry_reader)?;
            assert_eq!(mod_files.len(), 1);
            let relative_path = util::fs::path_relative_to_dir(&hello_file, repo_path)?;
            assert_eq!(mod_files[0], relative_path);

            Ok(())
        })
    }

    #[test]
    fn test_stager_list_untracked_dirs() -> Result<(), OxenError> {
        test::run_empty_stager_test(|stager, repo| {
            // Create entry_reader with no commits
            let commit_reader = CommitReader::new(&repo)?;
            let commit = commit_reader.head_commit()?;
            let entry_reader = CommitDirReader::new(&stager.repository, &commit)?;
            let repo_path = &stager.repository.path;
            let sub_dir = repo_path.join("training_data");
            std::fs::create_dir_all(&sub_dir)?;

            // Must have some sort of file in the dir to add it.
            test::write_txt_file_to_path(sub_dir.join("hi.txt"), "Hi")?;

            // Do not add...

            // List files
            let files = stager.list_untracked_directories(&entry_reader)?;
            assert_eq!(files.len(), 1);

            Ok(())
        })
    }

    #[test]
    fn test_stager_list_one_untracked_directory() -> Result<(), OxenError> {
        test::run_empty_stager_test(|stager, repo| {
            // Create entry_reader with no commits
            let commit_reader = CommitReader::new(&repo)?;
            let commit = commit_reader.head_commit()?;
            let entry_reader = CommitDirReader::new(&stager.repository, &commit)?;

            // Write two files to a sub directory
            let repo_path = &stager.repository.path;
            let sub_dir = repo_path.join("training_data");
            std::fs::create_dir_all(&sub_dir)?;

            let _ = test::add_txt_file_to_dir(&sub_dir, "Hello 1")?;
            let _ = test::add_txt_file_to_dir(&sub_dir, "Hello 2")?;

            // Do not add...

            // List files
            let files = stager.list_untracked_directories(&entry_reader)?;

            // There is one directory
            assert_eq!(files.len(), 1);

            // With two files
            assert_eq!(files[0].1, 2);

            Ok(())
        })
    }

    #[test]
    fn test_stager_add_dir_recursive() -> Result<(), OxenError> {
        test::run_training_data_repo_test_no_commits(|repo| {
            let stager = Stager::new(&repo)?;
            let commit_reader = CommitReader::new(&repo)?;
            let commit = commit_reader.head_commit()?;
            let entry_reader = CommitDirReader::new(&repo, &commit)?;

            // Write two files to a sub directory
            let repo_path = &stager.repository.path;
            let annotations_dir = PathBuf::from("annotations");
            let full_annotations_dir = repo_path.join(&annotations_dir);

            // Add the directory which has the structure
            // annotations/
            //   train/
            //     annotations.txt
            //     one_shot.txt
            //   test/
            //     annotations.txt
            stager.add(&full_annotations_dir, &entry_reader)?;

            // List dirs
            assert!(false);

            // let dirs = stager.list_added_directories()?;

            // // There is one directory
            // assert_eq!(dirs.len(), 1);

            // // With 3 recursive files
            // let added_dir = dirs
            //     .get(&StagedDirStats::from_path(annotations_dir))
            //     .unwrap();
            // assert_eq!(added_dir.num_files_staged, 3);

            Ok(())
        })
    }

    #[test]
    fn test_stager_modify_file_recursive() -> Result<(), OxenError> {
        test::run_training_data_repo_test_fully_committed(|repo| {
            let stager = Stager::new(&repo)?;
            let commit_reader = CommitReader::new(&repo)?;
            let commit = commit_reader.head_commit()?;
            let entry_reader = CommitDirReader::new(&repo, &commit)?;

            // Write two files to a sub directory
            let repo_path = &stager.repository.path;
            let one_shot_file = repo_path
                .join("annotations")
                .join("train")
                .join("one_shot.txt");

            // Add the directory which has the structure
            // annotations/
            //   train/
            //     one_shot.txt

            // Modify the committed file
            let one_shot_file = test::modify_txt_file(one_shot_file, "new content coming in hot")?;

            // List dirs
            let files = stager.list_modified_files(&entry_reader)?;

            // There is one modified file
            assert_eq!(files.len(), 1);

            // And it is
            let relative_path = util::fs::path_relative_to_dir(&one_shot_file, repo_path)?;
            assert_eq!(files[0], relative_path);

            Ok(())
        })
    }

    #[test]
    fn test_stager_list_untracked_directories_after_add() -> Result<(), OxenError> {
        test::run_empty_stager_test(|stager, repo| {
            // Create entry_reader with no commits
            let commit_reader = CommitReader::new(&repo)?;
            let commit = commit_reader.head_commit()?;
            let entry_reader = CommitDirReader::new(&stager.repository, &commit)?;

            // Create 2 sub directories, one with  Write two files to a sub directory
            let repo_path = &stager.repository.path;
            let train_dir = repo_path.join("train");
            std::fs::create_dir_all(&train_dir)?;
            let _ = test::add_img_file_to_dir(&train_dir, Path::new("data/test/images/cat_1.jpg"))?;
            let _ = test::add_img_file_to_dir(&train_dir, Path::new("data/test/images/dog_1.jpg"))?;
            let _ = test::add_img_file_to_dir(&train_dir, Path::new("data/test/images/cat_2.jpg"))?;
            let _ = test::add_img_file_to_dir(&train_dir, Path::new("data/test/images/dog_2.jpg"))?;

            let test_dir = repo_path.join("test");
            std::fs::create_dir_all(&test_dir)?;
            let _ = test::add_img_file_to_dir(&test_dir, Path::new("data/test/images/cat_3.jpg"))?;
            let _ = test::add_img_file_to_dir(&test_dir, Path::new("data/test/images/dog_3.jpg"))?;

            let valid_dir = repo_path.join("valid");
            std::fs::create_dir_all(&valid_dir)?;
            let _ = test::add_img_file_to_dir(&valid_dir, Path::new("data/test/images/dog_4.jpg"))?;

            let base_file_1 = test::add_txt_file_to_dir(repo_path, "Hello 1")?;
            let _base_file_2 = test::add_txt_file_to_dir(repo_path, "Hello 2")?;
            let _base_file_3 = test::add_txt_file_to_dir(repo_path, "Hello 3")?;

            // At first there should be 3 untracked
            let untracked_dirs = stager.list_untracked_directories(&entry_reader)?;
            assert_eq!(untracked_dirs.len(), 3);

            // Add the directory
            stager.add_dir(&train_dir, &entry_reader)?;
            // Add one file
            let _ = stager.add_file(&base_file_1, &entry_reader)?;

            // List the files
            assert!(false);

            // let added_files = stager.list_added_files()?;
            // let added_dirs = stager.list_added_directories()?;
            // let untracked_files = stager.list_untracked_files(&entry_reader)?;
            // let untracked_dirs = stager.list_untracked_directories(&entry_reader)?;

            // // There is 5 added file and 1 added dir
            // assert_eq!(added_files.len(), 5);
            // assert_eq!(added_dirs.len(), 1);

            // // There are 2 untracked files at the top level
            // assert_eq!(untracked_files.len(), 2);
            // // There are 2 untracked dirs at the top level
            // assert_eq!(untracked_dirs.len(), 2);

            Ok(())
        })
    }
}
