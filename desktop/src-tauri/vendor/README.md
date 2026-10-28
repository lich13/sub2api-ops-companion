# Native dependency patches

These crates retain their published APIs while the Tauri 2 GTK3 dependency chain is in use. Sources come from their crates.io releases; original licenses remain with each crate.

- `glib` 0.18.5, gtk-rs-core commit `42b9caf98e03ded086362d9653ca58fe94dc8658`: backport the mutable out-pointer fix from [gtk-rs-core #1343](https://github.com/gtk-rs/gtk-rs-core/pull/1343), addressing RUSTSEC-2024-0429. The version-based informational advisory may still identify this patched 0.18.5 source.
- `glib-macros` 0.18.5 and `gtk3-macros` 0.18.2: use maintained [`proc-macro-error3`](https://github.com/gamma0987/proc-macro-error3) with its Syn 2 feature in place of the retired `proc-macro-error` and `proc-macro-error2` branches.
- `tauri-plugin-opener` 2.7.2, plugins-workspace commit `44293a5722efcaf27060504e63b17aeba721b40f`: migrate the Linux/BSD file-manager integration to `zbus` 6, which includes its wire types and macros without the retired `zvariant`, `zvariant_derive` and `zbus_names` crates.
- `tauri-plugin-single-instance` 2.5.4: migrate its Linux D-Bus integration to `zbus` 6 with the same callback and instance-name behavior.

Remove a patch when the compatible upstream release includes its change. Do not replace the GLib family with an incompatible major version independently of Tauri.
