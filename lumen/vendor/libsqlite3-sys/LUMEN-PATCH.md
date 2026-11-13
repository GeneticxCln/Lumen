# Why this copy exists

This is `libsqlite3-sys` 0.39.0, unmodified except for two things: the declared package version
is `0.37.1`, and `sqlcipher/` was left out (Lumen does not use it).

`sqlx-sqlite 0.9.0` requires `libsqlite3-sys >=0.30.1, <0.38.0`, and every version in that range
bundles a SQLite with RUSTSEC-2026-0459 (CVE-2026-119375). 0.39.0 carries the fixed SQLite.
The root `Cargo.toml` patches the crate in through `[patch.crates-io]`; the version is spelled
`0.37.1` only so Cargo accepts it for sqlx's requirement.

Remove this directory and the `[patch.crates-io]` entry once sqlx ships a release that accepts
`libsqlite3-sys >=0.39`.
