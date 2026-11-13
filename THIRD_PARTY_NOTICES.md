# Third-party notices

The native engine source in `engine/` is maintained in this repository. Portions originate from cursor-byok, MIT, copyright 2026 leookun; that license is kept as `engine/LICENSE`. Dependency versions are pinned by `engine/Cargo.lock`.

The supervisor uses jsonc-parser 3.3.1, MIT, copyright Microsoft Corporation; its license is included in node_modules/jsonc-parser/LICENSE.md. Dependencies of the native executable retain their respective licenses in their source packages.

Magpie is a separate installed application; this package uses its public Agent Plugin interface. Compatibility was developed against v0.1.1220. This package does not bundle Magpie or Cursor.
