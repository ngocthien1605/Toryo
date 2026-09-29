<!-- TODO(owner): checklist items the team's reviewers add to the list below. -->
# X++ notes per role

**Planner** — list every object you create or extend (type, name, model) and every Chain of Command
method or event handler with its base method; name the labels you add. Say which acceptance criteria
only the cloud tier can prove (DB sync, SysTest): Verify cannot.

**Implementer** — edit the metadata XML in `Metadata/<Package>/<Model>/Ax<Type>/<Name>.xml` and keep
the structure Visual Studio writes (element order, X++ source in CDATA). A new object is a new file in
the right `Ax<Type>` folder. Do not edit descriptor XML unless the task says so. A CoC method calls
`next` exactly once.

**Reviewer** — check each acceptance criterion against the code, then:
- extensions and CoC only, `next` called once, nothing under ZZZ, no build project touched;
- labels instead of literals, EDTs on new fields, the prefix on new objects;
- `ttsbegin`/`ttscommit` balanced, no user interaction inside a transaction;
- set-based operations for many rows, field lists and `firstonly` in selects;
- a new menu item has a privilege; a new query range has an index that fits.
