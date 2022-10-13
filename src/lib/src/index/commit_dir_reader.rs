use crate::constants::HISTORY_DIR;
use crate::db;
use crate::error::OxenError;
use crate::index::{CommitDirEntryReader, CommitReader};
use crate::model::{Commit, CommitEntry, DirEntry};
use crate::util;

use rocksdb::{DBWithThreadMode, MultiThreaded};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::model::LocalRepository;

use super::path_db;

pub struct CommitDirReader {
    dir_db: DBWithThreadMode<MultiThreaded>,
    repository: LocalRepository,
    pub commit_id: String,
}

impl CommitDirReader {
    pub fn new(
        repository: &LocalRepository,
        commit: &Commit,
    ) -> Result<CommitDirReader, OxenError> {
        log::debug!("CommitDirReader::new() commit_id: {}", commit.id);
        let db_path = util::fs::oxen_hidden_dir(&repository.path)
            .join(HISTORY_DIR)
            .join(&commit.id)
            .join("dirs");
        let opts = db::opts::default();

        if !db_path.exists() {
            std::fs::create_dir_all(&db_path)?;
            // open it then lose scope to close it
            let _db: DBWithThreadMode<MultiThreaded> = DBWithThreadMode::open(&opts, &db_path)?;
        }

        Ok(CommitDirReader {
            dir_db: DBWithThreadMode::open_for_read_only(&opts, &db_path, true)?,
            repository: repository.to_owned(),
            commit_id: commit.id.to_owned(),
        })
    }

    /// For opening the entry reader from head, so that it opens and closes the commit db within the constructor
    pub fn new_from_head(repository: &LocalRepository) -> Result<CommitDirReader, OxenError> {
        let commit_reader = CommitReader::new(repository)?;
        let commit = commit_reader.head_commit()?;
        log::debug!("CommitDirReader::new_from_head() commit_id: {}", commit.id);
        CommitDirReader::new(repository, &commit)
    }

    pub fn list_committed_dirs(&self) -> Result<Vec<PathBuf>, OxenError> {
        path_db::list_paths(&self.dir_db, Path::new(""))
    }

    pub fn has_dir<P: AsRef<Path>>(&self, path: P) -> bool {
        path_db::has_entry(&self.dir_db, path)
    }

    pub fn num_entries(&self) -> Result<usize, OxenError> {
        let mut count = 0;
        for dir in self.list_committed_dirs()? {
            let commit_entry_dir =
                CommitDirEntryReader::new(&self.repository, &self.commit_id, &dir)?;
            count += commit_entry_dir.num_entries();
        }
        Ok(count)
    }

    pub fn list_files(&self) -> Result<Vec<PathBuf>, OxenError> {
        let mut paths: Vec<PathBuf> = vec![];
        for dir in self.list_committed_dirs()? {
            let commit_dir = CommitDirEntryReader::new(&self.repository, &self.commit_id, &dir)?;
            let mut files = commit_dir.list_files()?;
            paths.append(&mut files);
        }
        Ok(paths)
    }

    /// List entries in a vector when we need ordering
    pub fn list_entries(&self) -> Result<Vec<CommitEntry>, OxenError> {
        let mut paths: Vec<CommitEntry> = vec![];
        for dir in self.list_committed_dirs()? {
            let commit_dir = CommitDirEntryReader::new(&self.repository, &self.commit_id, &dir)?;
            let mut files = commit_dir.list_entries()?;
            paths.append(&mut files);
        }
        Ok(paths)
    }

    /// List entries in a set for quick lookup
    pub fn list_entries_set(&self) -> Result<HashSet<CommitEntry>, OxenError> {
        let mut paths: HashSet<CommitEntry> = HashSet::new();
        for dir in self.list_committed_dirs()? {
            let commit_dir = CommitDirEntryReader::new(&self.repository, &self.commit_id, &dir)?;
            let files = commit_dir.list_entries_set()?;
            paths.extend(files);
        }
        Ok(paths)
    }

    pub fn list_entry_page(
        &self,
        page_num: usize,
        page_size: usize,
    ) -> Result<Vec<CommitEntry>, OxenError> {
        let entries = self.list_entries()?;

        let start_page = if page_num == 0 { 0 } else { page_num - 1 };
        let start_idx = start_page * page_size;

        if (start_idx + page_size) < entries.len() {
            let subset: Vec<CommitEntry> = entries[start_idx..(start_idx + page_size)].to_vec();
            Ok(subset)
        } else if (start_idx < entries.len()) && (start_idx + page_size) >= entries.len() {
            let subset: Vec<CommitEntry> = entries[start_idx..entries.len()].to_vec();
            Ok(subset)
        } else {
            Ok(vec![])
        }
    }

    pub fn list_directory(
        &self,
        search_dir: &Path,
        page_num: usize,
        page_size: usize,
    ) -> Result<(Vec<DirEntry>, usize), OxenError> {
        let mut dir_paths: Vec<DirEntry> = vec![];
        for dir in self.list_committed_dirs()? {
            // log::debug!("LIST DIRECTORY considering committed dir: {:?} for search {:?}", dir, search_dir);
            if let Some(parent) = dir.parent() {
                if parent == search_dir
                    || (parent == Path::new("") && search_dir == Path::new("./"))
                {
                    dir_paths.push(DirEntry {
                        filename: String::from(dir.file_name().unwrap().to_str().unwrap()),
                        is_dir: true,
                    });
                }
            }
        }

        let mut file_paths: Vec<DirEntry> = vec![];
        let commit_dir_reader =
            CommitDirEntryReader::new(&self.repository, &self.commit_id, search_dir)?;
        let total = commit_dir_reader.num_entries() + dir_paths.len();
        for file in commit_dir_reader.list_entry_page(page_num, page_size)? {
            file_paths.push(DirEntry {
                filename: String::from(file.path.file_name().unwrap().to_str().unwrap()),
                is_dir: false,
            })
        }

        // Combine all paths, starting with dirs
        dir_paths.append(&mut file_paths);

        log::debug!(
            "list_directory page_num {} page_size {} total {}",
            page_num,
            page_size,
            total
        );
        if page_size < dir_paths.len() {
            let subset: Vec<DirEntry> = dir_paths[0..page_size].to_vec();
            Ok((subset, total))
        } else {
            Ok((dir_paths, total))
        }
    }

    pub fn has_prefix_in_dir(&self, prefix: &Path) -> bool {
        match self.list_entries() {
            Ok(entries) => entries
                .into_iter()
                .any(|entry| entry.path.starts_with(prefix)),
            _ => false,
        }
    }

    pub fn list_files_from_dir(&self, dir: &Path) -> Vec<CommitEntry> {
        match self.list_entries() {
            Ok(entries) => entries
                .into_iter()
                .filter(|entry| entry.path.starts_with(dir))
                .collect(),
            _ => {
                vec![]
            }
        }
    }

    pub fn has_file(&self, path: &Path) -> bool {
        if let (Some(parent), Some(file_name)) = (path.parent(), path.file_name()) {
            if let Ok(dir) = CommitDirEntryReader::new(&self.repository, &self.commit_id, parent) {
                return dir.has_file(file_name);
            }
        }
        false
    }

    pub fn get_entry(&self, path: &Path) -> Result<Option<CommitEntry>, OxenError> {
        if let (Some(parent), Some(file_name)) = (path.parent(), path.file_name()) {
            let dir = CommitDirEntryReader::new(&self.repository, &self.commit_id, parent)?;
            dir.get_entry(file_name)
        } else {
            Err(OxenError::file_has_no_parent(path))
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::command;
    use crate::error::OxenError;
    use crate::index::CommitDirReader;

    use crate::test;

    use std::path::Path;

    #[test]
    fn test_commit_dir_reader_check_if_file_exists() -> Result<(), OxenError> {
        test::run_training_data_repo_test_no_commits(|repo| {
            let filename = "labels.txt";
            let filepath = repo.path.join(filename);
            command::add(&repo, &filepath)?;
            let commit = command::commit(&repo, "Adding labels file")?.unwrap();

            let reader = CommitDirReader::new(&repo, &commit)?;
            let path = Path::new(filename);
            assert!(reader.has_file(path));

            Ok(())
        })
    }

    #[test]
    fn test_commit_dir_reader_list_top_level_directory() -> Result<(), OxenError> {
        test::run_training_data_repo_test_fully_committed(|repo| {
            let commits = command::log(&repo)?;
            let commit = commits.first().unwrap();

            let reader = CommitDirReader::new(&repo, commit)?;
            let (dir_entries, size) = reader.list_directory(Path::new("./"), 1, 10)?;
            for entry in dir_entries.iter() {
                println!("{:?}", entry);
            }

            assert_eq!(size, 5);
            assert_eq!(dir_entries.len(), 5);
            assert_eq!(
                dir_entries
                    .clone()
                    .into_iter()
                    .filter(|e| !e.is_dir)
                    .count(),
                2
            );
            assert_eq!(dir_entries.into_iter().filter(|e| e.is_dir).count(), 3);

            Ok(())
        })
    }

    #[test]
    fn test_commit_dir_reader_list_train_directory_full() -> Result<(), OxenError> {
        test::run_training_data_repo_test_fully_committed(|repo| {
            let commits = command::log(&repo)?;
            let commit = commits.first().unwrap();

            let reader = CommitDirReader::new(&repo, commit)?;
            let (dir_entries, size) = reader.list_directory(Path::new("train"), 1, 10)?;

            assert_eq!(size, 5);
            assert_eq!(dir_entries.len(), 5);

            Ok(())
        })
    }

    #[test]
    fn test_commit_dir_reader_list_train_sub_directory_full() -> Result<(), OxenError> {
        test::run_training_data_repo_test_fully_committed(|repo| {
            let commits = command::log(&repo)?;
            let commit = commits.first().unwrap();

            let reader = CommitDirReader::new(&repo, commit)?;
            let (dir_entries, size) =
                reader.list_directory(Path::new("annotations/train"), 1, 10)?;

            assert_eq!(size, 4);
            assert_eq!(dir_entries.len(), 4);

            Ok(())
        })
    }

    #[test]
    fn test_commit_dir_reader_list_train_directory_subset() -> Result<(), OxenError> {
        test::run_training_data_repo_test_fully_committed(|repo| {
            let commits = command::log(&repo)?;
            let commit = commits.first().unwrap();

            let reader = CommitDirReader::new(&repo, commit)?;
            let (dir_entries, size) = reader.list_directory(Path::new("train"), 2, 3)?;
            for entry in dir_entries.iter() {
                println!("{:?}", entry);
            }

            assert_eq!(size, 5);
            assert_eq!(dir_entries.len(), 2);

            Ok(())
        })
    }
}
