# Releasing batmon

Everything about cutting a release: the steps, what the tooling does behind
them, and how to write the changelog entries that become the release notes.

## 1. The steps

1. **Write changelog entries as part of each change**, under the
   `## [Unreleased]` heading at the top of [`CHANGELOG.md`](../CHANGELOG.md).
   Write them while the context is fresh, not at release time. See
   [§3](#3-writing-changelog-entries) for what to write.
2. **Preview the release:**

   ```bash
   cargo install cargo-release   # once
   cargo release patch           # or minor / major
   ```

   cargo-release is dry-run by default and has no interactive prompts. The
   preview shows the version bump and the changelog rollover, and fails if the
   Unreleased section is empty.
3. **Perform it:**

   ```bash
   cargo release patch -x
   ```

   `-x`/`--execute` is the confirmation step. Without it nothing happens, which
   is the most common way to conclude the release "didn't work".
4. **Wait for the workflow.** Pushing the tag starts
   [`release.yml`](../.github/workflows/release.yml), which builds both targets
   and publishes the GitHub release with its notes and binaries. Nothing else to
   do.

**Never create or publish a release in the GitHub web UI.** The workflow
creates the release itself and fails if one already exists for the tag.

Releases are cut from `master` with a clean working tree; cargo-release
refuses otherwise.

## 2. What happens behind the steps

### The changelog rolls over inside the release commit

`pre-release-replacements` in [`release.toml`](../release.toml) edit
`CHANGELOG.md` before cargo-release commits the version bump:

- `## [Unreleased] - ReleaseDate` becomes `## [0.9.0] - 2026-09-20`;
- a fresh, empty `## [Unreleased] - ReleaseDate` is inserted above it;
- the compare links at the bottom of the file gain a line for the new version.

Because this happens in the `chore(release)` commit, the tag already contains
the finished section. The file is cumulative: every release adds a section and
nothing is ever replaced.

### An empty section blocks the release

A `pre-release-hook` runs
[`scripts/release-notes.sh`](../scripts/release-notes.sh) on the section being
released and aborts if it has no entries. The check runs before the tag
exists, where adding the missing entry is cheap; after the tag is pushed,
fixing it means deleting and re-pushing the tag.

### The workflow publishes last

On a pushed `v*` tag, the workflow:

1. builds `x86_64` and `aarch64` musl binaries, packaging each with a `.sha256`;
2. extracts that version's section from `CHANGELOG.md` as the notes, failing if
   it is empty;
3. creates the release **as a draft** with every archive attached;
4. makes it public.

Publishing last matters because `install.sh` downloads from
`releases/latest/`, which points at a release the moment it is public. The
earlier workflow published first and built afterwards, so the one-line
installer 404'd until the binaries arrived, and a failed arm64 build could
leave a public release with amd64 only.

GitHub cannot trigger a workflow when a draft is saved (only on publish), so
there is no way to "write notes in the UI, then build". That is why the notes
live in `CHANGELOG.md` and the workflow owns the whole release.

## 3. Writing changelog entries

The notes are for **someone upgrading**, not someone reading the diff. Commit
messages already explain the code to future maintainers; the changelog tells
users what changed for them.

### What a version's section can contain

All parts are optional; use what the release needs, in this order.

| Part | Use it when |
| --- | --- |
| Lead paragraph | Big releases only. One to three sentences: what happened and why. |
| `**Upgrading:**` note | The user must *do* something — re-run `install.sh`, remove a dependency, restart the service. |
| `### Added` | Something new to use: a feature, command, alert, config option, install method. |
| `### Changed` | Something existing behaves differently: defaults, retention, performance, output, notifications. |
| `### Deprecated` | Still works, but will be removed later. |
| `### Removed` | Gone: a dependency users no longer need, a dropped option or command. |
| `### Fixed` | A bug that happened — or one that could have. |
| `### Security` | Anything about a vulnerability, including dependency bumps made for one. |

Omit empty headings. Start an entry with `**Behaviour change:**` when it could
surprise someone who upgrades without reading further.

### What gets an entry

The deciding question is **would someone upgrading notice this, or need to act
on it?** — not what type of commit it was.

- A `refactor` that cut memory from 28 MB to 6.6 MB gets an entry (the Rust
  port, v0.7.0). A `build` change that produced precompiled binaries gets one
  (v0.8.0).
- A rename, test restructuring, CI change, or most docs changes do not. They
  are already in `git log`, which the compare links cover.

**Hardening counts as a fix.** A change that closes a failure mode nobody has
hit yet belongs under Fixed if you can finish this sentence:

> Before this, ___ could happen when ___.

Write it with "could", so it claims nothing that did not happen:

```markdown
- An unreadable `capacity` could trigger a false "Battery Low" alert.
```

If the only honest description is "the logic is tighter" or "more robust",
with no concrete condition, it gets no entry. Bullets like "Improved
stability" tell the reader nothing and teach them to skip the notes.

Leave out commit hashes and file paths.

### When a release has only internal changes

Usually, don't release: nothing reaches users, and the work ships with the
next release that does. If you want to release anyway — to run a refactor on
real hardware, say — write one honest line, which also tells users upgrading
is optional:

```markdown
### Changed

- Internal refactoring only; no change in behaviour.
```

### Example

```markdown
## [0.7.0] - 2026-09-07

batmon is now a native Rust binary, replacing the TypeScript/Bun
implementation. Existing databases carry over unchanged.

**Upgrading:** re-run `./install.sh`. Bun and Node are no longer needed.

### Changed

- Median memory drops from 28.4 MB to 6.6 MB, and threads from ~15 to 6.

### Removed

- The Bun/Node runtime dependency.

### Fixed

- NaN or infinite sensor readings could trigger a false 0% critical alert.
```

## 4. Gotchas and recovery

- **The words `Unreleased` and `ReleaseDate` must not appear anywhere in
  `CHANGELOG.md` except the heading and link cargo-release manages.** The
  rollover replaces every occurrence, so a stray one in an entry gets rewritten
  into a version number or a date.
- **Dates are UTC.** Depending on the time of day, a release can carry a date
  one day off from your local calendar.
- **The publish job failed after creating the draft.** Re-running fails because
  the release exists. Finish it by hand:
  `gh release edit vX.Y.Z --draft=false` — or delete the draft and re-run the
  job.
- **A build failed.** Nothing is public yet. If it was a flake, re-run the
  failed jobs of the tag's workflow run. If the fix needs a new commit, the tag
  points at broken code: delete it (`git push --delete origin vX.Y.Z` and
  `git tag -d vX.Y.Z`), fix, and cut the next patch version. The skipped
  version number was never published, so nobody sees the gap. Before cutting
  it, move the dead version's entries back under the top heading and delete
  its heading and compare-link line — those entries still have not shipped.
- **Rehearsing the build.** Running the workflow manually (Actions → Release →
  Run workflow) builds both targets and uploads the artifacts, but never
  creates a release.
