<!-- TODO(owner): replace this example with a real work item. Keep `## Acceptance criteria` with one `- AC-n:` line per criterion (the parser reads only those). -->
# SJP-1234 — Credit hold reason on sales orders

## Goal
When a user puts a sales order on credit hold, they record a reason. The reason is shown on the
order header.

## Acceptance criteria
- AC-1: A new enum `SJPCreditHoldReason` and a field `SJPCreditHoldReason` on `SalesTable` (table extension, EDT on the field).
- AC-2: Putting an order on credit hold asks for a reason and stores it; cancelling the dialog leaves the order unchanged.
- AC-3: The reason is shown on the sales order header (form extension of `SalesTable`).
- AC-4: Every caption and message uses labels of `@SJP`, in en-US and ja.
- AC-5: Package ContosoCore compiles without new errors or warnings (Verify: compile only).

## Scope
- Model ContosoCore only; extensions and Chain of Command.

## Out of scope
- Existing holds (no data migration); security changes beyond the privilege of a new menu item.

## Verification
- The `build` gate proves AC-5. AC-2 and AC-3 need the database and a manual test in the
  environment, by the owner after the final approval and a deploy from Visual Studio.

## Allow protected
<!-- Only when THIS task must change a protected file (a build project, a descriptor, AGENTS.md…): one path per line, "- " first, e.g. the next line without its comment marks. Without such a line an agent's change to a protected file is restored. -->
<!-- - Metadata/ContosoCore/ContosoCore/Descriptor/ContosoCore.xml -->
