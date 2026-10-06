<!-- TODO(owner): which gate is the build here (the `tool: xpp` compile or scripts/build-model.ps1) and what a green build proves; the build project of each package, in build order. -->
# Build and verify

- Toryo, not you, runs the gates the repository declares, on your checkpoint commit: the `build` gate
  (a compile, or the team's build script — TODO(owner): say which). A green `build` proves what
  that build covers (a compile is **not** SysTest and **not** a database sync); deploying and the
  tests that need the database are the owner's, by hand, after the final gate.
- Build projects: `Projects/Build/<Package>.Build.rnrproj`, one per package — TODO(owner): list them.
  They are **protected**: never edit them, never add `Directory.Build.*` files.
- Do not change a model descriptor (`Descriptor/*.xml`) or package references unless the task says so.
- You cannot run MSBuild, xppc or a sync here, and you never deploy. Leave all of it to the gates;
  in a fix cycle, fix the reported diagnostics (file, line, code) first.
- Do not silence compiler or best-practice warnings with pragmas.
