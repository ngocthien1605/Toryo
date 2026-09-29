<!-- TODO(owner): the models and packages of this repository (model → package), and the exact ZZZ rule. -->
# D365FO models and packages

- Customise only through **extensions**: table, form, enum and EDT extensions, event handlers,
  Chain of Command. Never overlayer; never edit Microsoft or ISV source.
- Models here: TODO(owner) — e.g. `ContosoCore` (package ContosoCore). Each model uses only objects of
  the packages its descriptor references. A new package reference is a design decision: only when
  the task says so.
- **ZZZ**: `ZZZ*` models and folders are local experiments. Never create, change or commit anything
  under `**/ZZZ/**` — toryo refuses such changes.
- Source layout: `Metadata/<Package>/<Model>/Ax<Type>/<Object>.xml` (AxClass, AxTable, AxForm, …),
  one object per file, the file named after the object.

## X++ code

- New fields get an EDT (an existing one, or a new prefixed EDT) — no bare `str`/`real` fields.
- Every write runs inside `ttsbegin`/`ttscommit`, balanced on every path; no dialog or other user
  interaction inside a transaction.
- Many rows: set-based `insert_recordset`, `update_recordset`, `delete_from` where the table's
  overridden methods allow it; `select forupdate` only for rows you update.
- Selects name the fields they need and use `firstonly` for single rows.
