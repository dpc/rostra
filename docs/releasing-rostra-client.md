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
and license files. While generating downstream archives, temporary Cargo
configuration patches already generated internal dependencies because their
versions do not exist in the crates.io index yet. It then extracts all ten
archives into a temporary directory and creates an unrelated binary crate
outside the workspace. A narrowly scoped `[patch.crates-io]` table points each
Rostra dependency at one extracted archive. `cargo check` compiles that consumer
in a fresh target directory, and `cargo metadata` confirms that every Rostra
package came from the extracted artifacts rather than a sibling workspace
source.

This local patch is intentionally not a registry simulation. It proves that the
artifacts form a compilable `rostra-client` closure before their versions exist
on crates.io. The check also runs real `cargo publish --dry-run` verification
for the first six packages, which have no unpublished Rostra dependencies.


## Final registry validation and publication

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
this publication closure.
