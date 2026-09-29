<!-- TODO(owner): the company prefix, and any naming rule the team's reviewers enforce. -->
# Naming

- Every new object starts with the company prefix — TODO(owner): e.g. `SJP` — classes, tables,
  EDTs, enums, forms, menu items, security objects, label ids.
- Element extensions: `<BaseElement>.<Prefix>Extension` (e.g. `CustTable.SJPExtension`).
- Extension classes: `<Base><Prefix>_Extension`, `final`, with `[ExtensionOf(...)]`
  (e.g. `SalesTableSJP_Extension`). Event handler classes: `<Base><Prefix>EventHandler`.
- New fields and methods on standard objects: `<Prefix><Name>`, so a later Microsoft field or
  method never collides with them.
- Types and objects PascalCase, methods and variables camelCase; no other prefixes.
