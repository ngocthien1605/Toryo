# bin/

`toryo.exe` lives here on the Windows machine but is **not tracked by Git** (build artifact).

- Build natively on Windows (Rust toolchain installed): `cargo build --release`, then copy
  `target\release\toryo.exe` here.
- Or cross-build from Linux: `cargo build --release --target x86_64-pc-windows-gnu` and
  `x86_64-w64-mingw32-strip`.

`scripts/m0-smoke.ps1` and `docs/HUONG-DAN.md` expect the binary at `bin\toryo.exe`.
