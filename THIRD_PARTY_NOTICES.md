# Third-party notices

The native bridge derives from [leookun/cursor-byok](https://github.com/leookun/cursor-byok), MIT, copyright 2026 leookun, commit 31e9f5cbc98a7d28cffcb5efbb9323cd3e143354（0.3.4 实际验证的源码基线）. Its license is included as LICENSE. The exact source changes are in source.patch. Dependency versions are pinned by upstream Cargo.lock.

The supervisor uses jsonc-parser 3.3.1, MIT, copyright Microsoft Corporation; its license is included in node_modules/jsonc-parser/LICENSE.md. Dependencies of the native executable retain their respective licenses in their source packages.

Magpie is a separate installed application; this package uses its public Agent Plugin interface. Compatibility was developed against v0.1.1220. This package does not bundle Magpie or Cursor.
