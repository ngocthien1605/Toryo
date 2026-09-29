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
- Toryo Verify proves AC-5 (compile only). AC-2 and AC-3 need DB sync and a manual test in the
  cloud tier, by the owner after the final gate.
