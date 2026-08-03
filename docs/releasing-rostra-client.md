# Releasing the `rostra-client` crate closure

Run the durable local release check with:

```console
just check-client-release
```

The check packages these crates in publication order:

```text
rostra-util
rostra-util-error
rostra-util-fmt
rostra-util-dedup-chan
rostra-core
rostra-p2p-api
rostra-djot
rostra-p2p
rostra-client-db
rostra-client
```

For every crate, it generates the actual `.crate` archive and compares its file
list with the package's curated source, documentation, tests, examples, README,
and license files. The check first snapshots all package inputs, including
untracked or ignored files under `crates`, into an isolated temporary workspace.
This preserves the file-selection behavior of a live publish while preventing
temporary dependency resolution from rewriting the live `Cargo.lock` during
parallel CI jobs. While generating downstream archives, temporary Cargo
configuration patches use already generated internal dependencies because their
versions do not exist in the crates.io index yet. The check then extracts all ten
archives into a temporary directory and creates an unrelated binary crate outside
the workspace. A narrowly scoped `[patch.crates-io]` table points each Rostra
dependency at one extracted archive. `cargo check` compiles that consumer in a
fresh target directory, and `cargo metadata` confirms that every Rostra package
came from the extracted artifacts rather than a sibling workspace source.

This local patch is intentionally not a registry simulation. It proves that the
artifacts form a compilable `rostra-client` closure before their versions exist
on crates.io. It also compiles the extracted `rostra-p2p` artifact against the
extracted `rostra-core` artifact, so a synchronized version bump does not depend
on that version already existing in the registry. The check runs real
`cargo publish --dry-run` verification for the first six packages, which have no
unpublished Rostra dependencies.


## Final registry validation and publication

This workflow produces a crates-only SDK release, not a Rostra application
release. Record user-visible crate changes in the release commit's summary and
details; the project does not maintain a separate crate changelog. Tag the
successfully published release commit as `rostra-client-v<VERSION>`. Do not use
`v<VERSION>`: `v*` is reserved for product releases and triggers the GitHub
binary, DEB, and RPM release workflow when pushed there.

Create and review the release commit before final validation. Check out that
exact commit with a clean working tree, run `just check-client-release` and the
normal CI checks, and make no source changes until all registry uploads finish.
`cargo publish` repackages the live tree, so this rule keeps every uploaded
archive aligned with the reviewed and validated commit. If any source change is
needed, stop publication, amend and re-review the release commit, then restart
validation from the beginning.

Publish one package at a time in the order above. After crates.io exposes each
new package to its index, run the next package's real registry dry-run and then
publish it:

```console
cargo publish --dry-run -p rostra-djot
cargo publish -p rostra-djot

cargo publish --dry-run -p rostra-p2p
cargo publish -p rostra-p2p

cargo publish --dry-run -p rostra-client-db
cargo publish -p rostra-client-db

cargo publish --dry-run -p rostra-client
cargo publish -p rostra-client
```

Apply the same dry-run-then-publish sequence to the first six packages. The
final registry dry-runs prove what the local patch cannot: crates.io index
availability and normalized registry dependency resolution for each staged
package. A dry-run never uploads; only the subsequent real publish validates
credentials, package ownership, and the live upload service. Do not include
`rostra-web-ui` or `axum-dpc-static-assets` in this release; they are outside
this publication closure. After all ten versions are available and a fresh
registry-only consumer succeeds, tag the unchanged release commit and publish
that tag through the project's canonical Radicle remote.
