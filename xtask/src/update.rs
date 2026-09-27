//! `lcw-dev update`: bring this checkout to the latest `main` and keep the
//! installed runner in step with it, so staying current is one command rather
//! than a fetch, a checkout, a pull and a reinstall.
//!
//! It only ever **fast-forwards**, and that is the whole safety argument. A
//! fast-forward moves the branch pointer along history that already contains
//! every commit you had, so by construction it cannot lose work. Anything that
//! would need more — uncommitted edits to tracked files, or local commits that
//! `origin/main` lacks — stops with an explanation and leaves the checkout
//! exactly as it was, because resolving those takes a decision only a person
//! can make, and a tool that guesses there is a tool that eats work.

use std::path::Path;
use std::process::Command;

/// What `sync` did.
#[derive(Debug)]
pub struct Synced {
    pub before: String,
    pub after: String,
    /// The branch the checkout was on, when it was not already `main`.
    pub switched_from: Option<String>,
}

impl Synced {
    pub fn moved(&self) -> bool {
        self.before != self.after
    }
}

/// Fetch `origin/main`, switch to `main` if needed, and fast-forward.
pub fn sync(root: &Path) -> Result<Synced, String> {
    // Untracked files are left out on purpose: a fast-forward refuses by itself
    // if it would overwrite one, and blocking on every stray build artifact
    // would make the command useless in a working tree that is actually used.
    let dirty = git_out(root, &["status", "--porcelain", "--untracked-files=no"])?;
    if !dirty.trim().is_empty() {
        return Err(format!(
            "uncommitted changes to tracked files in {}:\n\n{}\n\n\
             `update` only fast-forwards, so it never overwrites work. Commit or \
             stash these, then run it again.",
            root.display(),
            indent(dirty.trim_end())
        ));
    }

    let before = rev(root, "HEAD")?;
    git(root, &["fetch", "--quiet", "origin", "main"])?;

    let branch = git_out(root, &["rev-parse", "--abbrev-ref", "HEAD"])?
        .trim()
        .to_string();
    // A detached HEAD reports itself as "HEAD"; the commit is what to return to.
    let return_to = if branch == "HEAD" {
        before.clone()
    } else {
        branch.clone()
    };
    let switched_from = if branch == "main" {
        None
    } else {
        git(root, &["checkout", "--quiet", "main"])?;
        Some(branch)
    };

    if let Err(why) = git(root, &["merge", "--ff-only", "--quiet", "origin/main"]) {
        if switched_from.is_some() {
            let _ = git(root, &["checkout", "--quiet", &return_to]);
        }
        return Err(format!(
            "local `main` has commits that `origin/main` does not, so it cannot be \
             fast-forwarded and was left untouched. Push or rebase them first.\n\
             git said: {why}"
        ));
    }

    let after = rev(root, "HEAD")?;
    Ok(Synced {
        before,
        after,
        switched_from,
    })
}

/// Whether anything under `path` differs between two commits.
pub fn changed(root: &Path, from: &str, to: &str, path: &str) -> bool {
    // `diff --quiet` exits 1 on a difference; any other failure is treated as
    // "changed" too, because the cost of a needless reinstall is seconds and the
    // cost of a skipped one is a runner silently out of step with its checkout.
    !Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["diff", "--quiet", from, to, "--", path])
        .status()
        .is_ok_and(|s| s.success())
}

/// One line per commit in `from..to`.
pub fn log(root: &Path, from: &str, to: &str) -> String {
    git_out(
        root,
        &[
            "log",
            "--oneline",
            "--no-decorate",
            &format!("{from}..{to}"),
        ],
    )
    .unwrap_or_default()
}

/// Where the checkout stands, from local refs only — no network, so it is
/// cheap enough for every `doctor` run. "Behind" is therefore as of the last
/// fetch, which `update` itself refreshes.
pub struct Describe {
    pub branch: String,
    pub short: String,
    pub behind: u32,
}

pub fn describe(root: &Path) -> Option<Describe> {
    let branch = git_out(root, &["rev-parse", "--abbrev-ref", "HEAD"]).ok()?;
    let short = git_out(root, &["rev-parse", "--short", "HEAD"]).ok()?;
    let behind = git_out(root, &["rev-list", "--count", "HEAD..origin/main"])
        .ok()
        .and_then(|n| n.trim().parse().ok())
        .unwrap_or(0);
    Some(Describe {
        branch: branch.trim().to_string(),
        short: short.trim().to_string(),
        behind,
    })
}

pub fn short(sha: &str) -> &str {
    &sha[..sha.len().min(7)]
}

fn rev(root: &Path, r: &str) -> Result<String, String> {
    git_out(root, &["rev-parse", "--verify", r]).map(|s| s.trim().to_string())
}

fn git(root: &Path, args: &[&str]) -> Result<(), String> {
    git_out(root, args).map(|_| ())
}

fn git_out(root: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .map_err(|e| format!("cannot run git: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

fn indent(s: &str) -> String {
    s.lines()
        .map(|l| format!("    {l}"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    //! Real repositories, not mocks: the behaviour under test *is* git's, and
    //! the one thing this module must never do — lose work — is only credible
    //! if it is checked against the real thing.

    use super::*;
    use std::path::PathBuf;

    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn run(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .expect("git runs");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn commit(dir: &Path, file: &str, content: &str) {
        let path = dir.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
        run(dir, &["add", "--all"]);
        run(
            dir,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@example.com",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "--quiet",
                "-m",
                file,
            ],
        );
    }

    /// An `origin` bare repository, a `work` clone that publishes to it, and a
    /// `user` clone — the checkout being updated.
    fn setup(tag: &str) -> (Scratch, PathBuf, PathBuf) {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("lcw-update-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let origin = root.join("origin.git");
        let work = root.join("work");
        let user = root.join("user");
        run(
            &root,
            &["init", "--quiet", "--bare", "-b", "main", "origin.git"],
        );
        run(&root, &["init", "--quiet", "-b", "main", "work"]);
        run(
            &work,
            &["remote", "add", "origin", origin.to_str().unwrap()],
        );
        commit(&work, "a.txt", "1");
        run(&work, &["push", "--quiet", "origin", "main"]);
        run(
            &root,
            &["clone", "--quiet", origin.to_str().unwrap(), "user"],
        );
        (Scratch(root), work, user)
    }

    fn publish(work: &Path, file: &str) {
        commit(work, file, file);
        run(work, &["push", "--quiet", "origin", "main"]);
    }

    fn head(dir: &Path) -> String {
        rev(dir, "HEAD").unwrap()
    }

    fn current_branch(dir: &Path) -> String {
        git_out(dir, &["rev-parse", "--abbrev-ref", "HEAD"])
            .unwrap()
            .trim()
            .to_string()
    }

    #[test]
    fn fast_forwards_to_origin_main() {
        let (_s, work, user) = setup("ff");
        let old = head(&user);
        publish(&work, "b.txt");

        let s = sync(&user).unwrap();
        assert_eq!(s.before, old);
        assert_eq!(s.after, head(&work));
        assert!(s.moved());
        assert!(s.switched_from.is_none());
        assert!(user.join("b.txt").is_file());
        assert!(log(&user, &s.before, &s.after).contains("b.txt"));
    }

    #[test]
    fn up_to_date_is_a_no_op() {
        let (_s, _work, user) = setup("noop");
        let s = sync(&user).unwrap();
        assert!(!s.moved());
    }

    #[test]
    fn uncommitted_edits_stop_it_and_survive_untouched() {
        let (_s, work, user) = setup("dirty");
        let old = head(&user);
        publish(&work, "b.txt");
        std::fs::write(user.join("a.txt"), "my edit").unwrap();

        let err = sync(&user).unwrap_err();
        assert!(err.contains("a.txt"), "{err}");
        assert_eq!(head(&user), old);
        assert_eq!(
            std::fs::read_to_string(user.join("a.txt")).unwrap(),
            "my edit"
        );
    }

    #[test]
    fn local_commits_are_never_rewritten() {
        let (_s, work, user) = setup("diverged");
        commit(&user, "mine.txt", "mine");
        let mine = head(&user);
        publish(&work, "theirs.txt");

        let err = sync(&user).unwrap_err();
        assert!(err.contains("fast-forward"), "{err}");
        assert_eq!(head(&user), mine);
    }

    #[test]
    fn a_failed_update_returns_to_the_branch_it_started_on() {
        let (_s, work, user) = setup("return");
        commit(&user, "mine.txt", "mine");
        run(&user, &["checkout", "--quiet", "-b", "feature"]);
        publish(&work, "theirs.txt");

        assert!(sync(&user).is_err());
        assert_eq!(current_branch(&user), "feature");
    }

    #[test]
    fn switches_to_main_from_another_branch() {
        let (_s, work, user) = setup("switch");
        run(&user, &["checkout", "--quiet", "-b", "old-work"]);
        publish(&work, "b.txt");

        let s = sync(&user).unwrap();
        assert_eq!(s.switched_from.as_deref(), Some("old-work"));
        assert_eq!(current_branch(&user), "main");
        assert_eq!(head(&user), head(&work));
    }

    #[test]
    fn untracked_files_do_not_block_and_are_kept() {
        let (_s, work, user) = setup("untracked");
        std::fs::write(user.join("scratch.log"), "notes").unwrap();
        publish(&work, "b.txt");

        assert!(sync(&user).unwrap().moved());
        assert!(user.join("scratch.log").is_file());
    }

    #[test]
    fn changed_is_scoped_to_a_path() {
        let (_s, work, user) = setup("changed");
        publish(&work, "xtask/src/main.rs");

        let s = sync(&user).unwrap();
        assert!(changed(&user, &s.before, &s.after, "xtask"));
        assert!(!changed(&user, &s.before, &s.after, "crates"));
    }

    #[test]
    fn describe_counts_commits_behind_the_last_fetch() {
        let (_s, work, user) = setup("describe");
        publish(&work, "b.txt");
        publish(&work, "c.txt");
        assert_eq!(describe(&user).unwrap().behind, 0, "not fetched yet");

        run(&user, &["fetch", "--quiet", "origin"]);
        let d = describe(&user).unwrap();
        assert_eq!(d.branch, "main");
        assert_eq!(d.behind, 2);
    }
}
