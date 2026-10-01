# Agent task queue

Small, self-contained tasks any coding agent (Cursor, Claude Code, a person) can
pick up. Each file is one task. **The folder is the queue**: the PR that finishes
a task deletes its file, so whatever is here is still open.

## The loop

1. **Pick** the lowest-numbered task you can do. Read `AGENTS.md` first; its
   rules (one-way crate dependencies, "front ends render, they never traverse",
   the Layer-1 precision rules) apply to every task.
2. **Branch** from the latest `main`: `cursor/T03-gpu-tests-in-ci` (any
   `<agent>/T<nn>-<slug>` works).
3. **Do exactly the task.** Each file names the files to touch, says what to do
   and lists **Done when** items that can be checked. If you need to touch a file
   the task does not name, say why in the PR. If the task is wrong or blocked,
   stop and open a *draft* PR that explains what you found.
4. **Verify** with the commands in `AGENTS.md` ("Delegated tasks") plus the
   task's own **Verify** section.
5. **Open a PR** titled `T03: <task title>`. In the body, copy the task's
   **Done when** list, tick each item and give its evidence: a test name, a
   command's last line, a screenshot for UI changes. Delete the task file in
   this PR.
6. **Do not merge.** The reviewer checks out the branch and re-runs everything.
   It also mutation-tests the new tests: it reverts the fix and confirms a test
   fails. It then merges, or leaves review comments for you to address on the
   same branch.

## Not delegated

Changes to how calls are resolved (Layer 1 in `lcw-adapter-*`, beyond what a task
explicitly asks), anything that bumps `CACHE_VERSION`, dependency upgrades and
releases stay with the reviewer. They change what every view reports, so they
need before-and-after measurements on real repositories.

## Writing a new task

Copy any task file. Keep it to one PR's worth. State **Why** (the problem, with
evidence), **Where** (files), **What to do**, **Done when** (checkable, including a
test that fails without the change), **Verify** and **Out of scope**.
