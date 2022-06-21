use crate::error::OxenError;
use crate::index::{RefReader, CommitReader, CommitEntryReader};
use crate::model::{Commit, LocalRepository};
use crate::util;

pub struct Merger {
    repository: LocalRepository
}

impl Merger {
    pub fn new(repo: &LocalRepository) -> Merger {
        Merger {
            repository: repo.to_owned()
        }
    }

    /// # Merge a branch name into the current checked out branch
    pub fn merge<S: AsRef<str>>(&self, branch_name: S) -> Result<Option<Commit>, OxenError> {
        let branch_name = branch_name.as_ref();
        let ref_reader = RefReader::new(&self.repository)?;
        let head_commit_id = ref_reader.head_commit_id()?;
        let merge_commit_id = ref_reader.get_commit_id_for_branch(branch_name)?
                                .ok_or_else(|| OxenError::commit_db_corrupted(branch_name))?;

        let commit_reader = CommitReader::new(&self.repository)?;
        let head_commit = commit_reader.get_commit_by_id(&head_commit_id)?
                            .ok_or_else(|| OxenError::commit_db_corrupted(&head_commit_id))?;
        let merge_commit = commit_reader.get_commit_by_id(&merge_commit_id)?
                            .ok_or_else(|| OxenError::commit_db_corrupted(&merge_commit_id))?;

        // TODO: This is just a fast forward merge, if we cannot traverse cleanly back from merge to HEAD
        //       we will have to find the lowest common ancestor and try to merge from there
        let head_commit_entry_reader = CommitEntryReader::new(&self.repository, &head_commit)?;
        let merge_commit_entry_reader = CommitEntryReader::new(&self.repository, &merge_commit)?;

        let head_entries = head_commit_entry_reader.list_entries_set()?;
        let merge_entries = merge_commit_entry_reader.list_entries_set()?;

        // Check if all the entries we want to merge exist
        for merge_entry in merge_entries.iter() {
            // Check if the one we want to merge is in HEAD
            if let Some(head_entry) = head_entries.get(&merge_entry) {
                // Check if the file contents is the same
                if head_entry.hash != merge_entry.hash {
                    // If it's different, we have to decide whether to take new, or try to merge automatically
                    return Err(OxenError::basic_str("TODO, merge strategy"));
                }
            } else {
                // We can just copy it over since it didn't exist before.
                let version_file = util::fs::version_path(&self.repository, &merge_entry);
                let dst_path = self.repository.path.join(&merge_entry.path);
                std::fs::copy(version_file, dst_path)?;
            }
        }

        // TODO: Remove all entries that are in HEAD but not in merge entries

        Ok(None)
    }
}


#[cfg(test)]
mod tests {
    use crate::command;
    use crate::error::OxenError;
    use crate::index::Merger;
    use crate::util;
    use crate::test;

    #[test]
    fn test_one_commit_fast_forward() -> Result<(), OxenError> {
        test::run_empty_local_repo_test(|repo| {
            // Write and commit hello file to main branch
            let og_branch = command::current_branch(&repo)?.unwrap();
            let hello_file = repo.path.join("hello.txt");
            util::fs::write_to_path(&hello_file, "Hello");
            command::add(&repo, hello_file)?;
            command::commit(&repo, "Adding hello file")?;

            // Branch to add world
            let branch_name = "add-world";
            command::create_checkout_branch(&repo, branch_name)?;

            let world_file = repo.path.join("world.txt");
            util::fs::write_to_path(&world_file, "World");
            command::add(&repo, &world_file)?;
            command::commit(&repo, "Adding world file")?;

            // Checkout and merge additions
            command::checkout(&repo, og_branch.name)?;
            
            // Make sure world file doesn't exist until we merge it in
            assert!(!world_file.exists());

            let merger = Merger::new(&repo);
            merger.merge(branch_name)?;

            // Now that we've merged in, world file should exist
            assert!(world_file.exists());

            Ok(())
        })
    }
}