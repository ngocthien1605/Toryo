<!-- TODO(owner): the build project of each package, in build order. -->
# Build and verify

- Toryo verifies with the X++ compiler only (`tool: xpp`): compile level, **no DB sync, no SysTest**.
  A green verify means "it compiles", not "it works".
- Build projects: `Projects/Build/<Package>.Build.rnrproj`, one per package — TODO(owner): list them.
  They are **protected**: never edit them, never add `Directory.Build.*` files.
- Do not change a model descriptor (`Descriptor/*.xml`) or package references unless the task says so.
- You cannot run MSBuild or xppc here. Leave the compile to Verify; in a fix cycle, fix the reported
  diagnostics (file, line, code) first.
- Do not silence compiler or best-practice warnings with pragmas.
