<!-- TODO(owner): the label file id of each model, and the languages kept in step (e.g. en-US, ja). -->
# Labels

- No user-visible literal text in X++ or metadata: captions, help texts and info/warning/error
  messages use labels (`"@SJP:CreditHoldReason"`).
- Label files: TODO(owner) — e.g. model `ContosoCore` → label file `SJP`.
- A new label goes into **every** language of its label file — TODO(owner): en-US, ja — with the
  same id.
- Reuse an existing label only when its text fits exactly; never change the text of a label that
  other objects use.
- Messages with values: placeholders `%1`, `%2` in the label and `strFmt("@SJP:Msg", value)` — never
  string concatenation.
