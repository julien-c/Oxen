//! # oxen checkout
//!
//! Checkout a branch or commit
//!

use std::path::Path;

use crate::core::df::tabular;
use crate::core::index::MergeConflictReader;
use crate::error::OxenError;
use crate::model::{Branch, LocalRepository};
use crate::opts::{DFOpts, RestoreOpts};
use crate::{api, command, util};

/// # Checkout a branch or commit id
/// This switches HEAD to point to the branch name or commit id,
/// it also updates all the local files to be from the commit that this branch references
pub async fn checkout<S: AsRef<str>>(
    repo: &LocalRepository,
    value: S,
) -> Result<Option<Branch>, OxenError> {
    let value = value.as_ref();
    log::debug!("--- CHECKOUT START {} ----", value);
    if api::local::branches::exists(repo, value)? {
        if api::local::branches::is_checked_out(repo, value) {
            println!("Already on branch {value}");
            return api::local::branches::get_by_name(repo, value);
        }

        println!("Checkout branch: {value}");
        api::local::branches::set_working_branch(repo, value).await?;
        api::local::branches::set_head(repo, value)?;
        api::local::branches::get_by_name(repo, value)
    } else {
        // If we are already on the commit, do nothing
        if api::local::branches::is_checked_out(repo, value) {
            eprintln!("Commit already checked out {value}");
            return Ok(None);
        }

        api::local::branches::set_working_commit_id(repo, value).await?;
        api::local::branches::set_head(repo, value)?;
        Ok(None)
    }
}

/// Create and checkout a branch
pub fn create_checkout<S: AsRef<str>>(
    repo: &LocalRepository,
    value: S,
) -> Result<Branch, OxenError> {
    api::local::branches::create_checkout(repo, value.as_ref())
}

/// # Checkout a file and take their changes
/// This overwrites the current file with the changes in the branch we are merging in
pub fn checkout_theirs(repo: &LocalRepository, path: impl AsRef<Path>) -> Result<(), OxenError> {
    let merger = MergeConflictReader::new(repo)?;
    let conflicts = merger.list_conflicts()?;
    log::debug!(
        "checkout_theirs {:?} conflicts.len() {}",
        path.as_ref(),
        conflicts.len()
    );

    // find the path that matches in the conflict, throw error if !found
    if let Some(conflict) = conflicts
        .iter()
        .find(|c| c.merge_entry.path == path.as_ref())
    {
        // Lookup the file for the merge commit entry and copy it over
        command::restore(
            repo,
            RestoreOpts::from_path_ref(path, conflict.merge_entry.commit_id.clone()),
        )
    } else {
        Err(OxenError::could_not_find_merge_conflict(path))
    }
}

/// # Checkout a file and take our changes
/// This overwrites the current file with the changes we had in our current branch
pub fn checkout_ours(repo: &LocalRepository, path: impl AsRef<Path>) -> Result<(), OxenError> {
    let merger = MergeConflictReader::new(repo)?;
    let conflicts = merger.list_conflicts()?;
    log::debug!(
        "checkout_ours {:?} conflicts.len() {}",
        path.as_ref(),
        conflicts.len()
    );

    // find the path that matches in the conflict, throw error if !found
    if let Some(conflict) = conflicts
        .iter()
        .find(|c| c.merge_entry.path == path.as_ref())
    {
        // Lookup the file for the base commit entry and copy it over
        command::restore(
            repo,
            RestoreOpts::from_path_ref(path, conflict.base_entry.commit_id.clone()),
        )
    } else {
        Err(OxenError::could_not_find_merge_conflict(path))
    }
}

/// # Combine Conflicting Tabular Data Files
/// This overwrites the current file with the changes in their file
pub fn checkout_combine<P: AsRef<Path>>(repo: &LocalRepository, path: P) -> Result<(), OxenError> {
    let merger = MergeConflictReader::new(repo)?;
    let conflicts = merger.list_conflicts()?;
    log::debug!(
        "checkout_combine checking path {:?} -> [{}] conflicts",
        path.as_ref(),
        conflicts.len()
    );
    // find the path that matches in the conflict, throw error if !found
    if let Some(conflict) = conflicts
        .iter()
        .find(|c| c.merge_entry.path == path.as_ref())
    {
        if util::fs::is_tabular(&conflict.base_entry.path) {
            let df_base_path = util::fs::version_path(repo, &conflict.base_entry);
            let df_base = tabular::read_df(df_base_path, DFOpts::empty())?;
            let df_merge_path = util::fs::version_path(repo, &conflict.merge_entry);
            let df_merge = tabular::read_df(df_merge_path, DFOpts::empty())?;

            log::debug!("GOT DF HEAD {}", df_base);
            log::debug!("GOT DF MERGE {}", df_merge);

            match df_base.vstack(&df_merge) {
                Ok(result) => {
                    log::debug!("GOT DF COMBINED {}", result);
                    match result.unique(None, polars::frame::UniqueKeepStrategy::First, None) {
                        Ok(mut uniq) => {
                            log::debug!("GOT DF COMBINED UNIQUE {}", uniq);
                            let output_path = repo.path.join(&conflict.base_entry.path);
                            tabular::write_df(&mut uniq, &output_path)
                        }
                        _ => Err(OxenError::basic_str("Could not uniq data")),
                    }
                }
                _ => Err(OxenError::basic_str(
                    "Could not combine data, make sure schema's match",
                )),
            }
        } else {
            Err(OxenError::basic_str(
                "Cannot use --combine on non-tabular data file.",
            ))
        }
    } else {
        Err(OxenError::could_not_find_merge_conflict(path))
    }
}

#[cfg(test)]
mod tests {
    use crate::api;
    use crate::command;
    use crate::constants::DEFAULT_BRANCH_NAME;
    use crate::error::OxenError;
    use crate::test;
    use crate::util;

    #[tokio::test]
    async fn test_command_checkout_non_existant_commit_id() -> Result<(), OxenError> {
        test::run_empty_local_repo_test_async(|repo| async move {
            // This shouldn't work
            let checkout_result = command::checkout(&repo, "non-existant").await;
            assert!(checkout_result.is_err());

            Ok(())
        })
        .await
    }

    #[tokio::test]
    async fn test_command_checkout_commit_id() -> Result<(), OxenError> {
        test::run_empty_local_repo_test_async(|repo| async move {
            // Write a hello file
            let hello_file = repo.path.join("hello.txt");
            util::fs::write_to_path(&hello_file, "Hello")?;

            // Stage a hello file
            command::add(&repo, &hello_file)?;
            // Commit the hello file
            let first_commit = command::commit(&repo, "Adding hello")?;

            // Write a world
            let world_file = repo.path.join("world.txt");
            util::fs::write_to_path(&world_file, "World")?;

            // Stage a world file
            command::add(&repo, &world_file)?;

            // Commit the world file
            command::commit(&repo, "Adding world")?;

            // We have the world file
            assert!(world_file.exists());

            // We checkout the previous commit
            command::checkout(&repo, first_commit.id).await?;

            // // Then we do not have the world file anymore
            assert!(!world_file.exists());

            // // Check status
            let status = command::status(&repo)?;
            assert!(status.is_clean());

            Ok(())
        })
        .await
    }

    #[tokio::test]
    async fn test_command_checkout_current_branch_name_does_nothing() -> Result<(), OxenError> {
        test::run_empty_local_repo_test_async(|repo| async move {
            // Write the first file
            let hello_file = repo.path.join("hello.txt");
            util::fs::write_to_path(&hello_file, "Hello")?;

            // Track & commit the file
            command::add(&repo, &hello_file)?;
            command::commit(&repo, "Added hello.txt")?;

            // Create and checkout branch
            let branch_name = "feature/world-explorer";
            api::local::branches::create_checkout(&repo, branch_name)?;
            command::checkout(&repo, branch_name).await?;

            Ok(())
        })
        .await
    }

    #[tokio::test]
    async fn test_cannot_checkout_branch_with_dots_in_name() -> Result<(), OxenError> {
        test::run_empty_local_repo_test_async(|repo| async move {
            // Write the first file
            let hello_file = repo.path.join("hello.txt");
            util::fs::write_to_path(&hello_file, "Hello")?;

            // Track & commit the file
            command::add(&repo, &hello_file)?;
            command::commit(&repo, "Added hello.txt")?;

            // Create and checkout branch
            let branch_name = "test..ing";
            let result = api::local::branches::create_checkout(&repo, branch_name);
            assert!(result.is_err());

            Ok(())
        })
        .await
    }

    #[tokio::test]
    async fn test_command_checkout_added_file() -> Result<(), OxenError> {
        test::run_empty_local_repo_test_async(|repo| async move {
            // Write the first file
            let hello_file = repo.path.join("hello.txt");
            util::fs::write_to_path(&hello_file, "Hello")?;

            // Track & commit the file
            command::add(&repo, &hello_file)?;
            command::commit(&repo, "Added hello.txt")?;

            // Get the original branch name
            let orig_branch = api::local::branches::current_branch(&repo)?.unwrap();

            // Create and checkout branch
            let branch_name = "feature/world-explorer";
            api::local::branches::create_checkout(&repo, branch_name)?;

            // Write a second file
            let world_file = repo.path.join("world.txt");
            util::fs::write_to_path(&world_file, "World")?;

            // Track & commit the second file in the branch
            command::add(&repo, &world_file)?;
            command::commit(&repo, "Added world.txt")?;

            // Make sure we have both commits after the initial
            let commits = api::local::commits::list(&repo)?;
            assert_eq!(commits.len(), 3);

            let branches = api::local::branches::list(&repo)?;
            assert_eq!(branches.len(), 2);

            // Make sure we have both files on disk in our repo dir
            assert!(hello_file.exists());
            assert!(world_file.exists());

            // Go back to the main branch
            command::checkout(&repo, orig_branch.name).await?;

            // The world file should no longer be there
            assert!(hello_file.exists());
            assert!(!world_file.exists());

            // Go back to the world branch
            command::checkout(&repo, branch_name).await?;
            assert!(hello_file.exists());
            assert!(world_file.exists());

            Ok(())
        })
        .await
    }

    #[tokio::test]
    async fn test_command_checkout_added_file_keep_untracked() -> Result<(), OxenError> {
        test::run_empty_local_repo_test_async(|repo| async move {
            // Write the first file
            let hello_file = repo.path.join("hello.txt");
            util::fs::write_to_path(&hello_file, "Hello")?;

            // Have another file lying around we will not remove
            let keep_file = repo.path.join("keep_me.txt");
            util::fs::write_to_path(&keep_file, "I am untracked, don't remove me")?;

            // Track & commit the file
            command::add(&repo, &hello_file)?;
            command::commit(&repo, "Added hello.txt")?;

            // Get the original branch name
            let orig_branch = api::local::branches::current_branch(&repo)?.unwrap();

            // Create and checkout branch
            let branch_name = "feature/world-explorer";
            api::local::branches::create_checkout(&repo, branch_name)?;

            // Write a second file
            let world_file = repo.path.join("world.txt");
            util::fs::write_to_path(&world_file, "World")?;

            // Track & commit the second file in the branch
            command::add(&repo, &world_file)?;
            command::commit(&repo, "Added world.txt")?;

            // Make sure we have both commits after the initial
            let commits = api::local::commits::list(&repo)?;
            assert_eq!(commits.len(), 3);

            let branches = api::local::branches::list(&repo)?;
            assert_eq!(branches.len(), 2);

            // Make sure we have all files on disk in our repo dir
            assert!(hello_file.exists());
            assert!(world_file.exists());
            assert!(keep_file.exists());

            // Go back to the main branch
            command::checkout(&repo, orig_branch.name).await?;

            // The world file should no longer be there
            assert!(hello_file.exists());
            assert!(!world_file.exists());
            assert!(keep_file.exists());

            // Go back to the world branch
            command::checkout(&repo, branch_name).await?;
            assert!(hello_file.exists());
            assert!(world_file.exists());
            assert!(keep_file.exists());

            Ok(())
        })
        .await
    }

    #[tokio::test]
    async fn test_command_checkout_modified_file() -> Result<(), OxenError> {
        test::run_empty_local_repo_test_async(|repo| async move {
            // Write the first file
            let hello_file = repo.path.join("hello.txt");
            util::fs::write_to_path(&hello_file, "Hello")?;

            // Track & commit the file
            command::add(&repo, &hello_file)?;
            command::commit(&repo, "Added hello.txt")?;

            // Get the original branch name
            let orig_branch = api::local::branches::current_branch(&repo)?.unwrap();

            // Create and checkout branch
            let branch_name = "feature/world-explorer";
            api::local::branches::create_checkout(&repo, branch_name)?;

            // Modify the file
            let hello_file = test::modify_txt_file(hello_file, "World")?;

            // Track & commit the change in the branch
            command::add(&repo, &hello_file)?;
            command::commit(&repo, "Changed file to world")?;

            // It should say World at this point
            assert_eq!(util::fs::read_from_path(&hello_file)?, "World");

            // Go back to the main branch
            command::checkout(&repo, orig_branch.name).await?;

            // The file contents should be Hello, not World
            log::debug!("HELLO FILE NAME: {:?}", hello_file);
            assert!(hello_file.exists());

            // It should be reverted back to Hello
            assert_eq!(util::fs::read_from_path(&hello_file)?, "Hello");

            Ok(())
        })
        .await
    }

    #[tokio::test]
    async fn test_command_checkout_modified_file_in_subdirectory() -> Result<(), OxenError> {
        test::run_select_data_repo_test_no_commits_async("annotations", |repo| async move {
            // Get the original branch name
            let orig_branch = api::local::branches::current_branch(&repo)?.unwrap();

            // Track & commit the file
            let one_shot_path = repo.path.join("annotations/train/one_shot.csv");
            command::add(&repo, &one_shot_path)?;
            command::commit(&repo, "Adding one shot")?;

            // Get OG file contents
            let og_content = util::fs::read_from_path(&one_shot_path)?;

            let branch_name = "feature/change-the-shot";
            api::local::branches::create_checkout(&repo, branch_name)?;

            let file_contents = "file,label\ntrain/cat_1.jpg,0\n";
            let one_shot_path = test::modify_txt_file(one_shot_path, file_contents)?;
            let status = command::status(&repo)?;
            assert_eq!(status.modified_files.len(), 1);
            status.print_stdout();
            command::add(&repo, &one_shot_path)?;
            let status = command::status(&repo)?;
            status.print_stdout();
            command::commit(&repo, "Changing one shot")?;

            // checkout OG and make sure it reverts
            command::checkout(&repo, orig_branch.name).await?;
            let updated_content = util::fs::read_from_path(&one_shot_path)?;
            assert_eq!(og_content, updated_content);

            // checkout branch again and make sure it reverts
            command::checkout(&repo, branch_name).await?;
            let updated_content = util::fs::read_from_path(&one_shot_path)?;
            assert_eq!(file_contents, updated_content);

            Ok(())
        })
        .await
    }

    #[tokio::test]
    async fn test_command_checkout_modified_file_from_fully_committed_repo() -> Result<(), OxenError>
    {
        test::run_select_data_repo_test_no_commits_async("annotations", |repo| async move {
            // Get the original branch name
            let orig_branch = api::local::branches::current_branch(&repo)?.unwrap();

            // Track & commit all the data
            let one_shot_path = repo.path.join("annotations/train/one_shot.csv");
            command::add(&repo, &repo.path)?;
            command::commit(&repo, "Adding one shot")?;

            // Get OG file contents
            let og_content = util::fs::read_from_path(&one_shot_path)?;

            let branch_name = "feature/modify-data";
            api::local::branches::create_checkout(&repo, branch_name)?;

            let file_contents = "file,label\ntrain/cat_1.jpg,0\n";
            let one_shot_path = test::modify_txt_file(one_shot_path, file_contents)?;
            let status = command::status(&repo)?;
            assert_eq!(status.modified_files.len(), 1);
            command::add(&repo, &one_shot_path)?;
            let status = command::status(&repo)?;
            assert_eq!(status.modified_files.len(), 0);
            assert_eq!(status.staged_files.len(), 1);

            let status = command::status(&repo)?;
            status.print_stdout();
            command::commit(&repo, "Changing one shot")?;

            // checkout OG and make sure it reverts
            command::checkout(&repo, orig_branch.name).await?;
            let updated_content = util::fs::read_from_path(&one_shot_path)?;
            assert_eq!(og_content, updated_content);

            // checkout branch again and make sure it reverts
            command::checkout(&repo, branch_name).await?;
            let updated_content = util::fs::read_from_path(&one_shot_path)?;
            assert_eq!(file_contents, updated_content);

            Ok(())
        })
        .await
    }

    #[tokio::test]
    async fn test_command_remove_dir_then_revert() -> Result<(), OxenError> {
        test::run_select_data_repo_test_no_commits_async("train", |repo| async move {
            // Get the original branch name
            let orig_branch = api::local::branches::current_branch(&repo)?.unwrap();

            // (dir already created in helper)
            let dir_to_remove = repo.path.join("train");
            let og_num_files = util::fs::rcount_files_in_dir(&dir_to_remove);

            // track the dir
            command::add(&repo, &dir_to_remove)?;
            command::commit(&repo, "Adding train dir")?;

            // Create a branch to make the changes
            let branch_name = "feature/removing-train";
            api::local::branches::create_checkout(&repo, branch_name)?;

            // Delete the directory from disk
            util::fs::remove_dir_all(&dir_to_remove)?;

            // Track the deletion
            command::add(&repo, &dir_to_remove)?;
            command::commit(&repo, "Removing train dir")?;

            // checkout OG and make sure it restores the train dir
            command::checkout(&repo, orig_branch.name).await?;
            assert!(dir_to_remove.exists());
            assert_eq!(util::fs::rcount_files_in_dir(&dir_to_remove), og_num_files);

            // checkout branch again and make sure it reverts
            command::checkout(&repo, branch_name).await?;
            assert!(!dir_to_remove.exists());

            Ok(())
        })
        .await
    }

    // Test the default clone (not --all or --shallow) can revert to files that are not local
    #[tokio::test]
    async fn test_checkout_deleted_after_clone() -> Result<(), OxenError> {
        test::run_training_data_fully_sync_remote(|local_repo, remote_repo| async move {
            let cloned_remote = remote_repo.clone();
            let og_commits = api::local::commits::list_all(&local_repo)?;

            // Clone with the --all flag
            test::run_empty_dir_test_async(|new_repo_dir| async move {
                let cloned_repo =
                    command::clone_url(&remote_repo.remote.url, &new_repo_dir.join("new_repo"))
                        .await?;

                // Make sure we have all the commit objects
                let cloned_commits = api::local::commits::list_all(&cloned_repo)?;
                assert_eq!(og_commits.len(), cloned_commits.len());

                // Make sure we set the HEAD file
                let head_commit = api::local::commits::head_commit(&cloned_repo);
                assert!(head_commit.is_ok());

                // We remove the test/ directory in one of the commits, so make sure we can go
                // back in the history to that commit
                let test_dir_path = cloned_repo.path.join("test");
                let commit = api::local::commits::first_by_message(&cloned_repo, "Adding test/")?;
                assert!(commit.is_some());
                assert!(!test_dir_path.exists());

                // checkout the commit
                command::checkout(&cloned_repo, &commit.unwrap().id).await?;
                // Make sure we restored the directory
                assert!(test_dir_path.exists());

                // list files in test_dir_path
                let test_dir_files = util::fs::list_files_in_dir(&test_dir_path);
                println!("test_dir_files: {:?}", test_dir_files.len());
                for file in test_dir_files.iter() {
                    println!("file: {:?}", file);
                }
                assert_eq!(test_dir_files.len(), 2);

                assert!(test_dir_path.join("1.jpg").exists());
                assert!(test_dir_path.join("2.jpg").exists());

                Ok(new_repo_dir)
            })
            .await?;

            Ok(cloned_remote)
        })
        .await
    }

    /*
    Checks workflow:

    $ oxen clone <URL>

    $ oxen checkout f412d166be1bead8 # earlier commit
    $ oxen checkout 55a4df7cd5d00eee # later commit

    Checkout commit: 55a4df7cd5d00eee
    Setting working directory to 55a4df7cd5d00eee
    IO(Os { code: 2, kind: NotFound, message: "No such file or directory" })

    */
    #[tokio::test]
    async fn test_clone_checkout_old_commit_checkout_new_commit() -> Result<(), OxenError> {
        test::run_training_data_fully_sync_remote(|_, remote_repo| async move {
            let remote_repo_copy = remote_repo.clone();

            test::run_empty_dir_test_async(|repo_dir| async move {
                let cloned_repo =
                    command::clone_url(&remote_repo.remote.url, &repo_dir.join("new_repo")).await?;

                let commits = api::local::commits::list(&cloned_repo)?;
                // iterate over commits in reverse order and checkout each one
                for commit in commits.iter().rev() {
                    println!(
                        "TEST checking out commit: {} -> '{}'",
                        commit.id, commit.message
                    );
                    command::checkout(&cloned_repo, &commit.id).await?;
                }

                Ok(repo_dir)
            })
            .await?;

            Ok(remote_repo_copy)
        })
        .await
    }

    #[tokio::test]
    async fn test_checkout_local_does_not_remove_untracked_files() -> Result<(), OxenError> {
        // Push the Remote Repo
        test::run_empty_sync_repo_test(|_, remote_repo| async move {
            let remote_repo_copy = remote_repo.clone();

            // Clone Repo to User A
            test::run_empty_dir_test_async(|user_a_repo_dir| async move {
                let user_a_repo_dir_copy = user_a_repo_dir.join("user_a_repo");
                let user_a_repo =
                    command::clone_url(&remote_repo.remote.url, &user_a_repo_dir_copy).await?;

                // Create a new branch
                let branch_name = "test-branch";
                api::local::branches::create_checkout(&user_a_repo, branch_name)?;

                // Back to main
                command::checkout(&user_a_repo, DEFAULT_BRANCH_NAME).await?;

                // Create some untracked files...
                let file_1 = user_a_repo.path.join("file_1.txt");
                let dir_1 = user_a_repo.path.join("dir_1");
                let file_in_dir_1 = dir_1.join("file_in_dir_1.txt");
                let dir_2 = user_a_repo.path.join("dir_2");
                let subdir_2 = dir_2.join("subdir_2");
                let file_in_dir_2 = subdir_2.join("file_in_dir_2.txt");
                let file_in_subdir_2 = subdir_2.join("file_in_subdir_2.txt");

                // Create the files and dirs
                std::fs::create_dir(&dir_1)?;
                std::fs::create_dir(&dir_2)?;
                std::fs::create_dir(&subdir_2)?;

                test::write_txt_file_to_path(&file_1, "this is file 1")?;
                test::write_txt_file_to_path(&file_in_dir_1, "this is file in dir 1")?;
                test::write_txt_file_to_path(&file_in_dir_2, "this is file in dir 2")?;
                test::write_txt_file_to_path(&file_in_subdir_2, "this is file in subdir 2")?;

                // Switch back over to the other branch
                command::checkout(&user_a_repo, branch_name).await?;

                // Files should exist
                assert!(file_1.exists());
                assert!(file_in_dir_1.exists());
                assert!(file_in_dir_2.exists());
                assert!(file_in_subdir_2.exists());

                Ok(user_a_repo_dir_copy)
            })
            .await?;

            Ok(remote_repo_copy)
        })
        .await
    }

    #[tokio::test]
    async fn test_checkout_remote_does_not_remove_untracked_files() -> Result<(), OxenError> {
        test::run_training_data_fully_sync_remote(|_local_repo, remote_repo| async move {
            // Create additional branch on remote repo before clone

            let cloned_remote = remote_repo.clone();

            // Clone with the --all flag
            test::run_empty_dir_test_async(|new_repo_dir| async move {
                let cloned_repo = command::deep_clone_url(
                    &remote_repo.remote.url,
                    &new_repo_dir.join("new_repo"),
                )
                .await?;

                // Create untracked files
                let file_1 = cloned_repo.path.join("file_1.txt");
                let dir_1 = cloned_repo.path.join("dir_1");
                let file_in_dir_1 = dir_1.join("file_in_dir_1.txt");
                let dir_2 = cloned_repo.path.join("dir_2");
                let subdir_2 = dir_2.join("subdir_2");
                let file_in_dir_2 = subdir_2.join("file_in_dir_2.txt");

                // Create the files and dirs
                std::fs::create_dir(&dir_1)?;
                std::fs::create_dir(&dir_2)?;
                std::fs::create_dir(&subdir_2)?;

                test::write_txt_file_to_path(&file_1, "this is file 1")?;
                test::write_txt_file_to_path(&file_in_dir_1, "this is file in dir 1")?;
                test::write_txt_file_to_path(&file_in_dir_2, "this is file in dir 2")?;

                // Create a new branch after cloning (so we have to fetch the new commit from the remote)

                let branch_name = "test-branch";
                api::remote::branches::create_from_or_get(
                    &remote_repo,
                    branch_name,
                    DEFAULT_BRANCH_NAME,
                )
                .await?;

                command::fetch(&cloned_repo).await?;

                // Checkout the new branch
                command::checkout(&cloned_repo, branch_name).await?;

                // Files should exist
                assert!(file_1.exists());
                assert!(file_in_dir_1.exists());
                assert!(file_in_dir_2.exists());

                Ok(new_repo_dir)
            })
            .await?;

            Ok(cloned_remote)
        })
        .await
    }

    #[tokio::test]
    async fn test_checkout_old_commit_does_not_overwrite_untracked_files() -> Result<(), OxenError>
    {
        test::run_training_data_fully_sync_remote(|_local_repo, remote_repo| async move {
            // Create additional branch on remote repo before clone
            let branch_name = "test-branch";
            api::remote::branches::create_from_or_get(
                &remote_repo,
                branch_name,
                DEFAULT_BRANCH_NAME,
            )
            .await?;

            let cloned_remote = remote_repo.clone();

            // Clone with the --all flag
            test::run_empty_dir_test_async(|new_repo_dir| async move {
                let cloned_repo = command::deep_clone_url(
                    &remote_repo.remote.url,
                    &new_repo_dir.join("new_repo"),
                )
                .await?;

                let test_dir_path = cloned_repo.path.join("test");
                let commit = api::local::commits::first_by_message(&cloned_repo, "Adding test/")?;

                // Create untracked files
                let file_1 = cloned_repo.path.join("file_1.txt");
                let dir_1 = cloned_repo.path.join("dir_1");
                let file_in_dir_1 = dir_1.join("file_in_dir_1.txt");
                let dir_2 = cloned_repo.path.join("dir_2");
                let subdir_2 = dir_2.join("subdir_2");
                let file_in_dir_2 = subdir_2.join("file_in_dir_2.txt");

                // Create the files and dirs
                std::fs::create_dir(&dir_1)?;
                std::fs::create_dir(&dir_2)?;
                std::fs::create_dir(&subdir_2)?;

                test::write_txt_file_to_path(&file_1, "this is file 1")?;
                test::write_txt_file_to_path(&file_in_dir_1, "this is file in dir 1")?;
                test::write_txt_file_to_path(&file_in_dir_2, "this is file in dir 2")?;

                assert!(commit.is_some());
                assert!(!test_dir_path.exists());

                // checkout the commit
                command::checkout(&cloned_repo, &commit.unwrap().id).await?;
                // Make sure we restored the directory
                assert!(test_dir_path.exists());
                // Make sure the untracked files are still there
                assert!(file_1.exists());
                assert!(file_in_dir_1.exists());
                assert!(file_in_dir_2.exists());

                Ok(new_repo_dir)
            })
            .await?;

            Ok(cloned_remote)
        })
        .await
    }
}
