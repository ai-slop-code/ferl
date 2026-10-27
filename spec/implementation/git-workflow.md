# Local Git workflow for implementation agents

The owner requires a local Git repository, one branch per numbered implementation part, and a committed merge into `main` when that part is verified. This workflow is part of each part's completion gate. Initialization, local commits and local merges are authorized by this instruction.

## 1. Initialize or inspect the repository

Before implementation, inspect the repository root, current branch, working tree, index and existing history. Preserve existing user changes and commits.

If the project does not yet have a repository, initialize it at the ferl project root:

```sh
git init -b main
```

If Git discovers an enclosing repository rather than this project's repository, resolve that layout before creating a nested repository. If this project already has a repository, reuse it. If `main` is absent, establish it from the reviewed existing baseline without overwriting branches or discarding history.

Use the existing configured Git author/committer identity. If identity is missing, obtain the intended identity rather than inventing a person or modifying global Git configuration.

For a newly initialized repository, create a baseline commit on `main` before starting part 01:

1. Inspect all existing files and add appropriate `.gitignore` entries for build output, local secrets/configuration, container data and generated test captures. Keep source, specifications, examples without secrets and the application's Cargo.lock eligible for tracking.
2. Explicitly stage the reviewed specifications, implementation instructions and other intended baseline files. Inspect the staged diff; do not blindly stage unrelated files or generated data.
3. Commit with a message such as `chore: initialize ferl specification baseline`.

The initial baseline is the setup exception to the rule that implementation work happens on part branches. An existing valid baseline does not need to be recreated.

## 2. Start or resume a part branch

Use `implementation/NN-<part-slug>`, derived from the selected instruction filename. Examples:

```text
implementation/01-foundation
implementation/02-persistence
implementation/10-observability-and-lifecycle
```

Start a new part from the current verified `main` with a clean working tree and index:

```sh
git switch main
git switch -c implementation/01-foundation
```

Adjust the branch name for the selected part. Record the base commit and branch in progress.md. Inspect an existing part branch and resume its work rather than resetting, overwriting or duplicating it. Check history as well as progress.md to distinguish incomplete work from a part already merged.

Implement source, tests, migrations and the part's documentation on that branch. Coherent checkpoint commits are permitted; incomplete checkpoint work must remain on the part branch. Preserve unrelated changes; do not discard, stash or include them automatically just to obtain a clean tree.

## 3. Verify and commit the part

Complete the part's functionality, prescribed checks and relevant regression checks. Review the complete diff against its `main` baseline for accidental scope changes, missing files and generated/secret data.

Update progress.md with exact observed results, actual acceptance-test references and status **Verified (awaiting merge)**. Stage the intended implementation and handoff changes explicitly, review the staged diff and commit. Use an informative message such as:

```text
feat(part-01): add server foundation and health endpoints
```

Record the resulting part-tip commit for integration. Before merging, ensure the working tree/index are clean and the current `main` is an ancestor of the part branch. If `main` advanced independently, merge it into the part branch, resolve conflicts there, rerun the checks affected by integration, and commit before trying the final merge. Do not assume previous results verify changed code.

A failed or unexecuted required check blocks integration. Record the blocker and preserve work on the part branch; do not merge an unverified implementation into `main`.

## 4. Merge and close the handoff atomically

Use an explicit merge commit so each completed part is visible in local history. After the verified part commit exists and `main` is its ancestor:

```sh
git switch main
git merge --no-ff --no-commit implementation/01-foundation
```

The intentional `--no-commit` stop allows the handoff to be finalized in the same merge commit:

1. Confirm the merged implementation matches the verified part tip. If there is an unexpected conflict or code change, return integration work to the part branch and verify it before completion.
2. In progress.md, change this part to **Complete** and record its branch, base commit, verified part-tip commit and verification evidence. Only the handoff metadata should differ from the verified part tree at this point.
3. Stage that progress update, review the merge result, and create the merge commit:

```sh
git add -- spec/implementation/progress.md
git commit -m "merge: complete part 01 — foundation"
```

Adjust the part number and summary. This commit records both the actual integration and its completed handoff, without an extra implementation change directly on `main`. The merge commit's own hash cannot be embedded in itself; obtain it afterward for the final response and reference it from a later part if needed.

Verify that the part-tip commit is an ancestor of `main`, the expected merge commit exists, and the working tree/index are clean. If the only difference from tested code is progress metadata, history/tree checks are sufficient; rerun affected implementation checks if code or build inputs changed. Finish on `main` and retain the part branch for traceability.

If a session stops after verification or during a pending merge, the next session must inspect Git's actual state and resume integration before starting another part. A complete status visible in an uncommitted merge is not proof of completion.

## 5. Completion and final response

A part is complete only when:

- Its required implementation and checks pass.
- Its source, tests, migrations and handoff are committed.
- Its verified part branch is merged into local `main`.
- Git history and progress.md agree and the final working tree/index are clean.

Include the part branch, verified implementation commit, merge commit and final Git status in the final handoff. If Git integration is blocked, report the part as unmerged rather than complete. Work remains local; remote setup or pushing requires a separate request.
