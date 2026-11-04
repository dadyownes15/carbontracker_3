//! Finds a process's descendants, used to select the GPUs a run uses.

/// `root` and its current descendants, read from `/proc`. On systems without
/// `/proc`, returns only `root`.
pub fn process_tree(root: u32) -> Vec<u32> {
    let mut tree = vec![root];
    let Ok(entries) = std::fs::read_dir("/proc") else { return tree };
    let parents: Vec<(u32, u32)> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let pid: u32 = entry.file_name().to_str()?.parse().ok()?;
            let stat = std::fs::read_to_string(entry.path().join("stat")).ok()?;
            Some((pid, parent(&stat)?))
        })
        .collect();
    let mut frontier = vec![root];
    while let Some(next) = frontier.pop() {
        for &(pid, ppid) in &parents {
            if ppid == next && !tree.contains(&pid) {
                tree.push(pid);
                frontier.push(pid);
            }
        }
    }
    tree
}

/// Parses the parent pid from a `/proc/<pid>/stat` line. The command name
/// may contain spaces and parentheses.
fn parent(stat: &str) -> Option<u32> {
    stat.rsplit_once(')')?.1.split_whitespace().nth(1)?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_parent_follows_the_command_whatever_it_holds() {
        assert_eq!(parent("4242 (python3 (train) x) S 4200 4242 4200 0"), Some(4200));
        assert_eq!(parent("garbage"), None);
    }

    #[test]
    fn the_tree_holds_its_root() {
        assert!(process_tree(std::process::id()).contains(&std::process::id()));
    }
}
