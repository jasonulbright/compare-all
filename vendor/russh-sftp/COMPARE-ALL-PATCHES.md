# Compare-All patches

This directory contains `russh-sftp` 2.4.0, from crates.io. The upstream
source commit is `e145c1f7ece99f41f558949ef59731f2cd1a9dfe`; the crate keeps
its Apache-2.0 license in `LICENSE`.

Compare-All patches two limits in the SFTP client:

- `client::run` passes its configured maximum packet length to the inbound
  packet reader. Upstream passed `u32::MAX`, allowing a server supplied length
  prefix to request a multi-gigabyte allocation.
- `SftpSession::read_dir` caps one listing at 100,000 entries and 64 MiB of
  entry text, and appends each page without recopying all earlier pages.
- The unused wasm runtime adapter is removed; Compare-All is a native desktop
  application, and the adapter used unsafe code inside the vendored workspace.

The caller also wraps the SSH stream with the same packet-length ceiling. The
duplicate check protects the allocation boundary if the dependency is changed
or its packet parser is refactored later.
