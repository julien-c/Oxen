//! Entries are the files and directories that are stored in a commit.
//!

use crate::error::OxenError;
use crate::model::entry::metadata_entry::MetaData;
use crate::util;
use crate::view::entry::ResourceVersion;
use rayon::prelude::*;

use crate::core;
use crate::core::index::{CommitDirEntryReader, CommitEntryReader, CommitReader};
use crate::model::{Commit, CommitEntry, EntryDataType, LocalRepository, MetaDataEntry};
use crate::view::{PaginatedDirEntries, StatusMessage};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Get the entry for a given path in a commit.
/// Could be a file or a directory.
pub fn get_meta_entry(
    repo: &LocalRepository,
    commit: &Commit,
    path: &Path,
) -> Result<MetaDataEntry, OxenError> {
    let entry_reader = CommitEntryReader::new(repo, commit)?;
    let commit_reader = CommitReader::new(repo)?;
    // Check if the path is a dir
    if entry_reader.has_dir(path) {
        meta_entry_from_dir(repo, commit, path, &commit_reader, &commit.id)
    } else {
        let parent = path.parent().ok_or(OxenError::file_has_no_parent(path))?;
        let base_name = path.file_name().ok_or(OxenError::file_has_no_name(path))?;
        let dir_entry_reader = CommitDirEntryReader::new(repo, &commit.id, parent)?;
        let entry = dir_entry_reader
            .get_entry(base_name)?
            .ok_or(OxenError::entry_does_not_exist_in_commit(path, &commit.id))?;
        meta_entry_from_commit_entry(repo, &entry, &commit_reader, &commit.id)
    }
}

/// Get a DirEntry summing up the size of all files in a directory
/// and finding the latest commit within the directory
pub fn meta_entry_from_dir(
    repo: &LocalRepository,
    commit: &Commit,
    path: &Path,
    commit_reader: &CommitReader,
    revision: &str,
) -> Result<MetaDataEntry, OxenError> {
    // We cache the latest commit and size for each file in the directory after commit
    let latest_commit_path =
        core::cache::cachers::repo_size::dir_latest_commit_path(repo, commit, path);
    let latest_commit = match util::fs::read_from_path(&latest_commit_path) {
        Ok(id) => commit_reader.get_commit_by_id(id)?,
        Err(_) => {
            // cache failed, go compute it
            compute_latest_commit(repo, commit, path, commit_reader)?
        }
    };

    let total_size_path = core::cache::cachers::repo_size::dir_size_path(repo, commit, path);
    let total_size = match util::fs::read_from_path(&total_size_path) {
        Ok(total_size_str) => total_size_str
            .parse::<u64>()
            .map_err(|_| OxenError::basic_str("Could not get cached total size of dir"))?,
        Err(_) => {
            // cache failed, go compute it
            compute_dir_size(repo, commit, path)?
        }
    };

    let base_name = path.file_name().ok_or(OxenError::file_has_no_name(path))?;
    return Ok(MetaDataEntry {
        filename: String::from(base_name.to_string_lossy()),
        is_dir: true,
        size: total_size,
        latest_commit,
        data_type: EntryDataType::Dir,
        mime_type: "inode/directory".to_string(),
        extension: util::fs::file_extension(path),
        resource: Some(ResourceVersion {
            version: revision.to_string(),
            path: String::from(path.to_string_lossy()),
        }),
        meta: MetaData {
            text: None,
            image: None,
            video: None,
            audio: None,
            tabular: None,
        },
    });
}

fn compute_latest_commit(
    repo: &LocalRepository,
    commit: &Commit,
    path: &Path,
    commit_reader: &CommitReader,
) -> Result<Option<Commit>, OxenError> {
    let entry_reader = CommitEntryReader::new(repo, commit)?;
    let commits: HashMap<String, Commit> = HashMap::new();
    let mut latest_commit = Some(commit.to_owned());
    // This lists all the committed dirs
    let dirs = entry_reader.list_dirs()?;
    for dir in dirs {
        // Have to make sure we are in a subset of the dir (not really a tree structure)
        if dir.starts_with(path) {
            let entry_reader = CommitDirEntryReader::new(repo, &commit.id, &dir)?;
            for entry in entry_reader.list_entries()? {
                let commit = if commits.contains_key(&entry.commit_id) {
                    Some(commits[&entry.commit_id].clone())
                } else {
                    commit_reader.get_commit_by_id(&entry.commit_id)?
                };

                if latest_commit.is_none() {
                    latest_commit = commit.clone();
                }

                if latest_commit.as_ref().unwrap().timestamp > commit.as_ref().unwrap().timestamp {
                    latest_commit = commit.clone();
                }
            }
        }
    }
    Ok(latest_commit)
}

fn compute_dir_size(
    repo: &LocalRepository,
    commit: &Commit,
    path: &Path,
) -> Result<u64, OxenError> {
    let entry_reader = CommitEntryReader::new(repo, commit)?;
    let mut total_size: u64 = 0;
    // This lists all the committed dirs
    let dirs = entry_reader.list_dirs()?;
    for dir in dirs {
        // Have to make sure we are in a subset of the dir (not really a tree structure)
        if dir.starts_with(path) {
            let entry_reader = CommitDirEntryReader::new(repo, &commit.id, &dir)?;
            for entry in entry_reader.list_entries()? {
                total_size += entry.num_bytes;
            }
        }
    }
    Ok(total_size)
}

pub fn meta_entry_from_commit_entry(
    repo: &LocalRepository,
    entry: &CommitEntry,
    commit_reader: &CommitReader,
    revision: &str,
) -> Result<MetaDataEntry, OxenError> {
    let size = util::fs::version_file_size(repo, entry)?;
    let latest_commit = commit_reader.get_commit_by_id(&entry.commit_id)?.unwrap();

    let base_name = entry
        .path
        .file_name()
        .ok_or(OxenError::file_has_no_name(&entry.path))?;

    let version_path = util::fs::version_path(repo, entry);
    return Ok(MetaDataEntry {
        filename: String::from(base_name.to_string_lossy()),
        is_dir: false,
        size,
        latest_commit: Some(latest_commit),
        data_type: util::fs::file_data_type(&version_path),
        mime_type: util::fs::file_mime_type(&version_path),
        extension: util::fs::file_extension(&version_path),
        resource: Some(ResourceVersion {
            version: revision.to_string(),
            path: String::from(entry.path.to_string_lossy()),
        }),
        meta: MetaData {
            text: None,
            image: None,
            video: None,
            audio: None,
            tabular: None,
        },
    });
}

/// Commit entries are always files, not directories. Will return None if the path is a directory.
pub fn get_commit_entry(
    repo: &LocalRepository,
    commit: &Commit,
    path: &Path,
) -> Result<Option<CommitEntry>, OxenError> {
    let reader = CommitEntryReader::new(repo, commit)?;
    reader.get_entry(path)
}

pub fn list_all(repo: &LocalRepository, commit: &Commit) -> Result<Vec<CommitEntry>, OxenError> {
    let reader = CommitEntryReader::new(repo, commit)?;
    reader.list_entries()
}

pub fn count_for_commit(repo: &LocalRepository, commit: &Commit) -> Result<usize, OxenError> {
    let reader = CommitEntryReader::new(repo, commit)?;
    reader.num_entries()
}

pub fn list_page(
    repo: &LocalRepository,
    commit: &Commit,
    page: &usize,
    page_size: &usize,
) -> Result<Vec<CommitEntry>, OxenError> {
    let reader = CommitEntryReader::new(repo, commit)?;
    reader.list_entry_page(*page, *page_size)
}

/// List all files and directories in a directory given a specific commit
// This is wayyyy more complicated that it needs to be because we have these two separate dbs....
pub fn list_directory(
    repo: &LocalRepository,
    commit: &Commit,
    directory: &Path,
    revision: &str,
    page: usize,
    page_size: usize,
) -> Result<PaginatedDirEntries, OxenError> {
    let entry_reader = CommitEntryReader::new(repo, commit)?;
    let commit_reader = CommitReader::new(repo)?;

    // List the directories first, then the files
    let mut dir_paths: Vec<MetaDataEntry> = vec![];
    for dir in entry_reader.list_dirs()? {
        // log::debug!("LIST DIRECTORY considering committed dir: {:?} for search {:?}", dir, search_dir);
        if let Some(parent) = dir.parent() {
            if parent == directory || (parent == Path::new("") && directory == Path::new("./")) {
                dir_paths.push(meta_entry_from_dir(
                    repo,
                    commit,
                    &dir,
                    &commit_reader,
                    revision,
                )?);
            }
        }
    }
    log::debug!("list_directory got dir_paths {}", dir_paths.len());

    // Once we know how many directories we have we can calculate the offset for the files
    let mut file_paths: Vec<MetaDataEntry> = vec![];
    let dir_entry_reader = CommitDirEntryReader::new(repo, &commit.id, directory)?;
    log::debug!("list_directory counting entries...");
    let total = dir_entry_reader.num_entries() + dir_paths.len();
    log::debug!("list_directory got {} total entries", total);
    let offset = dir_paths.len();
    for entry in dir_entry_reader.list_entry_page_with_offset(page, page_size, offset)? {
        file_paths.push(meta_entry_from_commit_entry(
            repo,
            &entry,
            &commit_reader,
            revision,
        )?)
    }
    log::debug!("list_directory got file_paths {}", file_paths.len());

    let total_pages = (total as f64 / page_size as f64).ceil() as usize;

    // Combine all paths, starting with dirs if there are enough, else just files
    let start_page = if page == 0 { 0 } else { page - 1 };
    let start_idx = start_page * page_size;
    log::debug!(
        "list_directory start_idx {start_idx} page_size {page_size} dir_paths.len() {}",
        dir_paths.len()
    );
    let mut entries = if dir_paths.len() < start_idx {
        file_paths
    } else {
        dir_paths.append(&mut file_paths);
        dir_paths
    };

    if entries.len() >= page_size {
        entries = entries[0..page_size].to_vec();
    }

    log::debug!(
        "list_directory {:?} page {} page_size {} total {} total_pages {}",
        directory,
        page,
        page_size,
        total,
        total_pages,
    );

    let resource = Some(ResourceVersion {
        path: directory.to_str().unwrap().to_string(),
        version: revision.to_string(),
    });

    Ok(PaginatedDirEntries {
        status: StatusMessage::resource_found(),
        entries,
        resource,
        page_size,
        page_number: page,
        total_pages,
        total_entries: total,
    })
}

/// Given a list of entries, compute the total in bytes size of all entries.
pub fn compute_entries_size(entries: &[CommitEntry]) -> Result<u64, OxenError> {
    let total_size: u64 = entries.into_par_iter().map(|e| e.num_bytes).sum();
    Ok(total_size)
}

/// Given a list of entries, group them by their parent directory.
pub fn group_entries_to_parent_dirs(entries: &[CommitEntry]) -> HashMap<PathBuf, Vec<CommitEntry>> {
    let mut results: HashMap<PathBuf, Vec<CommitEntry>> = HashMap::new();

    for entry in entries.iter() {
        if let Some(parent) = entry.path.parent() {
            results
                .entry(parent.to_path_buf())
                .or_default()
                .push(entry.clone());
        }
    }

    results
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use crate::api;
    use crate::command;
    use crate::core;
    use crate::error::OxenError;
    use crate::test;
    use crate::util;

    #[test]
    fn test_api_local_entries_list_all() -> Result<(), OxenError> {
        test::run_training_data_repo_test_no_commits(|repo| {
            // (file already created in helper)
            let file_to_add = repo.path.join("labels.txt");

            // Commit the file
            command::add(&repo, file_to_add)?;
            let commit = command::commit(&repo, "Adding labels file")?;

            let entries = api::local::entries::list_all(&repo, &commit)?;
            assert_eq!(entries.len(), 1);

            Ok(())
        })
    }

    #[test]
    fn test_api_local_entries_count_one_for_commit() -> Result<(), OxenError> {
        test::run_training_data_repo_test_no_commits(|repo| {
            // (file already created in helper)
            let file_to_add = repo.path.join("labels.txt");

            // Commit the file
            command::add(&repo, file_to_add)?;
            let commit = command::commit(&repo, "Adding labels file")?;

            let count = api::local::entries::count_for_commit(&repo, &commit)?;
            assert_eq!(count, 1);

            Ok(())
        })
    }

    #[test]
    fn test_api_local_entries_count_many_for_commit() -> Result<(), OxenError> {
        test::run_training_data_repo_test_no_commits(|repo| {
            // (files already created in helper)
            let dir_to_add = repo.path.join("train");
            let num_files = util::fs::rcount_files_in_dir(&dir_to_add);

            // Commit the dir
            command::add(&repo, &dir_to_add)?;
            let commit = command::commit(&repo, "Adding training data")?;
            let count = api::local::entries::count_for_commit(&repo, &commit)?;
            assert_eq!(count, num_files);

            Ok(())
        })
    }

    #[test]
    fn test_api_local_entries_count_many_dirs() -> Result<(), OxenError> {
        test::run_training_data_repo_test_no_commits(|repo| {
            // (files already created in helper)
            let num_files = util::fs::rcount_files_in_dir(&repo.path);

            // Commit the dir
            command::add(&repo, &repo.path)?;
            let commit = command::commit(&repo, "Adding all data")?;

            let count = api::local::entries::count_for_commit(&repo, &commit)?;
            assert_eq!(count, num_files);

            Ok(())
        })
    }

    #[test]
    fn test_get_meta_entry_dir() -> Result<(), OxenError> {
        test::run_training_data_repo_test_fully_committed(|repo| {
            let commits = api::local::commits::list(&repo)?;
            let commit = commits.first().unwrap();

            let path = Path::new("annotations").join("train");
            let entry = api::local::entries::get_meta_entry(&repo, commit, &path)?;

            assert!(entry.is_dir);
            assert_eq!(entry.filename, "train");
            assert_eq!(Path::new(&entry.resource.unwrap().path), path);

            Ok(())
        })
    }

    #[test]
    fn test_get_meta_entry_file() -> Result<(), OxenError> {
        test::run_training_data_repo_test_fully_committed(|repo| {
            let commits = api::local::commits::list(&repo)?;
            let commit = commits.first().unwrap();

            let path = test::test_nlp_classification_csv();
            let entry = api::local::entries::get_meta_entry(&repo, commit, path)?;

            assert!(!entry.is_dir);
            assert_eq!(entry.filename, "test.tsv");
            assert_eq!(
                Path::new(&entry.resource.unwrap().path),
                test::test_nlp_classification_csv()
            );

            Ok(())
        })
    }

    #[test]
    fn test_list_top_level_directory() -> Result<(), OxenError> {
        test::run_training_data_repo_test_fully_committed(|repo| {
            let commits = api::local::commits::list(&repo)?;
            let commit = commits.first().unwrap();

            let paginated = api::local::entries::list_directory(
                &repo,
                commit,
                Path::new("./"),
                &commit.id,
                1,
                10,
            )?;
            let dir_entries = paginated.entries;
            let size = paginated.total_entries;
            for entry in dir_entries.iter() {
                println!("{entry:?}");
            }

            assert_eq!(size, 7);
            assert_eq!(dir_entries.len(), 7);
            assert_eq!(
                dir_entries
                    .clone()
                    .into_iter()
                    .filter(|e| !e.is_dir)
                    .count(),
                2
            );
            assert_eq!(dir_entries.into_iter().filter(|e| e.is_dir).count(), 5);

            Ok(())
        })
    }

    #[test]
    fn test_list_train_directory_full() -> Result<(), OxenError> {
        test::run_training_data_repo_test_fully_committed(|repo| {
            let commits = api::local::commits::list(&repo)?;
            let commit = commits.first().unwrap();

            let paginated = api::local::entries::list_directory(
                &repo,
                commit,
                Path::new("train"),
                &commit.id,
                1,
                10,
            )?;
            let dir_entries = paginated.entries;
            let size = paginated.total_entries;

            assert_eq!(size, 5);
            assert_eq!(dir_entries.len(), 5);

            Ok(())
        })
    }

    #[test]
    fn test_list_train_sub_directory_full() -> Result<(), OxenError> {
        test::run_training_data_repo_test_fully_committed(|repo| {
            let commits = api::local::commits::list(&repo)?;
            let commit = commits.first().unwrap();

            let paginated = api::local::entries::list_directory(
                &repo,
                commit,
                Path::new("annotations/train"),
                &commit.id,
                1,
                10,
            )?;
            let dir_entries = paginated.entries;
            let size = paginated.total_entries;

            assert_eq!(size, 4);
            assert_eq!(dir_entries.len(), 4);

            Ok(())
        })
    }

    #[test]
    fn test_list_train_directory_subset() -> Result<(), OxenError> {
        test::run_training_data_repo_test_fully_committed(|repo| {
            let commits = api::local::commits::list(&repo)?;
            let commit = commits.first().unwrap();

            let paginated = api::local::entries::list_directory(
                &repo,
                commit,
                Path::new("train"),
                &commit.id,
                2,
                3,
            )?;
            let dir_entries = paginated.entries;
            let total_entries = paginated.total_entries;

            for entry in dir_entries.iter() {
                println!("{entry:?}");
            }

            assert_eq!(total_entries, 5);
            assert_eq!(dir_entries.len(), 2);

            Ok(())
        })
    }

    #[test]
    fn test_list_train_directory_1_exactly_ten() -> Result<(), OxenError> {
        test::run_empty_local_repo_test(|repo| {
            // Create 8 directories
            for n in 0..8 {
                let dirname = format!("dir_{}", n);
                let dir_path = repo.path.join(dirname);
                util::fs::create_dir_all(&dir_path)?;
                let filename = "data.txt";
                let filepath = dir_path.join(filename);
                util::fs::write(&filepath, format!("Hi {}", n))?;
            }
            // Create 2 files
            let filename = "labels.txt";
            let filepath = repo.path.join(filename);
            util::fs::write(filepath, "hello world")?;

            let filename = "README.md";
            let filepath = repo.path.join(filename);
            util::fs::write(filepath, "readme....")?;

            // Add and commit all the dirs and files
            command::add(&repo, &repo.path)?;
            let commit = command::commit(&repo, "Adding all the data")?;

            // Run the compute cache
            let force = true;
            core::cache::commit_cacher::run_all(&repo, &commit, force)?;

            let page_number = 1;
            let page_size = 10;

            let paginated = api::local::entries::list_directory(
                &repo,
                &commit,
                Path::new("."),
                &commit.id,
                page_number,
                page_size,
            )?;
            assert_eq!(paginated.total_entries, 10);
            assert_eq!(paginated.total_pages, 1);
            assert_eq!(paginated.entries.len(), 10);

            Ok(())
        })
    }

    #[test]
    fn test_list_train_directory_exactly_ten_page_two() -> Result<(), OxenError> {
        test::run_empty_local_repo_test(|repo| {
            // Create 8 directories
            for n in 0..8 {
                let dirname = format!("dir_{}", n);
                let dir_path = repo.path.join(dirname);
                util::fs::create_dir_all(&dir_path)?;
                let filename = "data.txt";
                let filepath = dir_path.join(filename);
                util::fs::write(&filepath, format!("Hi {}", n))?;
            }
            // Create 2 files
            let filename = "labels.txt";
            let filepath = repo.path.join(filename);
            util::fs::write(filepath, "hello world")?;

            let filename = "README.md";
            let filepath = repo.path.join(filename);
            util::fs::write(filepath, "readme....")?;

            // Add and commit all the dirs and files
            command::add(&repo, &repo.path)?;
            let commit = command::commit(&repo, "Adding all the data")?;

            // Run the compute cache
            let force = true;
            core::cache::commit_cacher::run_all(&repo, &commit, force)?;

            let page_number = 2;
            let page_size = 10;

            let paginated = api::local::entries::list_directory(
                &repo,
                &commit,
                Path::new("."),
                &commit.id,
                page_number,
                page_size,
            )?;
            assert_eq!(paginated.total_entries, 10);
            assert_eq!(paginated.total_pages, 1);
            assert_eq!(paginated.entries.len(), 0);

            Ok(())
        })
    }

    #[test]
    fn test_list_train_directory_nine_entries_page_size_ten() -> Result<(), OxenError> {
        test::run_empty_local_repo_test(|repo| {
            // Create 7 directories
            for n in 0..7 {
                let dirname = format!("dir_{}", n);
                let dir_path = repo.path.join(dirname);
                util::fs::create_dir_all(&dir_path)?;
                let filename = "data.txt";
                let filepath = dir_path.join(filename);
                util::fs::write(&filepath, format!("Hi {}", n))?;
            }
            // Create 2 files
            let filename = "labels.txt";
            let filepath = repo.path.join(filename);
            util::fs::write(filepath, "hello world")?;

            let filename = "README.md";
            let filepath = repo.path.join(filename);
            util::fs::write(filepath, "readme....")?;

            // Add and commit all the dirs and files
            command::add(&repo, &repo.path)?;
            let commit = command::commit(&repo, "Adding all the data")?;

            // Run the compute cache
            let force = true;
            core::cache::commit_cacher::run_all(&repo, &commit, force)?;

            let page_number = 1;
            let page_size = 10;

            let paginated = api::local::entries::list_directory(
                &repo,
                &commit,
                Path::new("."),
                &commit.id,
                page_number,
                page_size,
            )?;
            assert_eq!(paginated.total_entries, 9);
            assert_eq!(paginated.total_pages, 1);
            assert_eq!(paginated.entries.len(), 9);

            Ok(())
        })
    }

    #[test]
    fn test_list_train_directory_eleven_entries_page_size_ten() -> Result<(), OxenError> {
        test::run_empty_local_repo_test(|repo| {
            // Create 9 directories
            for n in 0..9 {
                let dirname = format!("dir_{}", n);
                let dir_path = repo.path.join(dirname);
                util::fs::create_dir_all(&dir_path)?;
                let filename = "data.txt";
                let filepath = dir_path.join(filename);
                util::fs::write(&filepath, format!("Hi {}", n))?;
            }
            // Create 2 files
            let filename = "labels.txt";
            let filepath = repo.path.join(filename);
            util::fs::write(filepath, "hello world")?;

            let filename = "README.md";
            let filepath = repo.path.join(filename);
            util::fs::write(filepath, "readme....")?;

            // Add and commit all the dirs and files
            command::add(&repo, &repo.path)?;
            let commit = command::commit(&repo, "Adding all the data")?;

            // Run the compute cache
            let force = true;
            core::cache::commit_cacher::run_all(&repo, &commit, force)?;

            let page_number = 1;
            let page_size = 10;

            let paginated = api::local::entries::list_directory(
                &repo,
                &commit,
                Path::new("."),
                &commit.id,
                page_number,
                page_size,
            )?;
            assert_eq!(paginated.total_entries, 11);
            assert_eq!(paginated.total_pages, 2);
            assert_eq!(paginated.entries.len(), page_size);

            Ok(())
        })
    }

    #[test]
    fn test_list_train_directory_many_dirs_many_files() -> Result<(), OxenError> {
        test::run_empty_local_repo_test(|repo| {
            // Create many directories
            let num_dirs = 32;
            for n in 0..num_dirs {
                let dirname = format!("dir_{}", n);
                let dir_path = repo.path.join(dirname);
                util::fs::create_dir_all(&dir_path)?;
                let filename = "data.txt";
                let filepath = dir_path.join(filename);
                util::fs::write(&filepath, format!("Hi {}", n))?;
            }

            // Create many files
            let num_files = 45;
            for n in 0..num_files {
                let filename = format!("file_{}.txt", n);
                let filepath = repo.path.join(filename);
                util::fs::write(filepath, format!("helloooo {}", n))?;
            }

            // Add and commit all the dirs and files
            command::add(&repo, &repo.path)?;
            let commit = command::commit(&repo, "Adding all the data")?;

            // Run the compute cache
            let force = true;
            core::cache::commit_cacher::run_all(&repo, &commit, force)?;

            let page_number = 1;
            let page_size = 10;

            let paginated = api::local::entries::list_directory(
                &repo,
                &commit,
                Path::new("."),
                &commit.id,
                page_number,
                page_size,
            )?;
            assert_eq!(paginated.total_entries, num_dirs + num_files);
            assert_eq!(paginated.total_pages, 8);
            assert_eq!(paginated.entries.len(), page_size);

            Ok(())
        })
    }

    #[test]
    fn test_list_train_directory_one_dir_many_files_page_2() -> Result<(), OxenError> {
        test::run_empty_local_repo_test(|repo| {
            // Create one directory
            let dir_path = repo.path.join("lonely_dir");
            util::fs::create_dir_all(&dir_path)?;
            let filename = "data.txt";
            let filepath = dir_path.join(filename);
            util::fs::write(&filepath, "All the lonely directories")?;

            // Create many files
            let num_files = 45;
            for n in 0..num_files {
                let filename = format!("file_{}.txt", n);
                let filepath = repo.path.join(filename);
                util::fs::write(filepath, format!("helloooo {}", n))?;
            }

            // Add and commit all the dirs and files
            command::add(&repo, &repo.path)?;
            let commit = command::commit(&repo, "Adding all the data")?;

            // Run the compute cache
            let force = true;
            core::cache::commit_cacher::run_all(&repo, &commit, force)?;

            let page_number = 2;
            let page_size = 10;

            let paginated = api::local::entries::list_directory(
                &repo,
                &commit,
                Path::new("."),
                &commit.id,
                page_number,
                page_size,
            )?;

            assert_eq!(paginated.total_entries, num_files + 1);
            assert_eq!(paginated.total_pages, 5);
            assert_eq!(paginated.entries.len(), page_size);

            Ok(())
        })
    }

    #[test]
    fn test_list_train_directory_many_dir_some_files_page_2() -> Result<(), OxenError> {
        test::run_empty_local_repo_test(|repo| {
            // Create many directories
            let num_dirs = 9;
            for n in 0..num_dirs {
                let dirname = format!("dir_{}", n);
                let dir_path = repo.path.join(dirname);
                util::fs::create_dir_all(&dir_path)?;
                let filename = "data.txt";
                let filepath = dir_path.join(filename);
                util::fs::write(&filepath, format!("Hi {}", n))?;
            }

            // Create many files
            let num_files = 8;
            for n in 0..num_files {
                let filename = format!("file_{}.txt", n);
                let filepath = repo.path.join(filename);
                util::fs::write(filepath, format!("helloooo {}", n))?;
            }

            // Add and commit all the dirs and files
            command::add(&repo, &repo.path)?;
            let commit = command::commit(&repo, "Adding all the data")?;

            // Run the compute cache
            let force = true;
            core::cache::commit_cacher::run_all(&repo, &commit, force)?;

            let page_number = 2;
            let page_size = 10;

            let paginated = api::local::entries::list_directory(
                &repo,
                &commit,
                Path::new("."),
                &commit.id,
                page_number,
                page_size,
            )?;

            assert_eq!(paginated.total_entries, num_files + num_dirs);
            assert_eq!(paginated.total_pages, 2);
            assert_eq!(paginated.entries.len(), 7);

            Ok(())
        })
    }
}
