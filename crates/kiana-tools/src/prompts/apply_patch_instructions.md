# apply_patch format

Every call must contain exactly one patch. The patch is plain text (not JSON):

```
*** Begin Patch
*** Add File: path/to/new_file.py
+first line
+second line
*** Delete File: path/to/obsolete_file.py
*** Update File: path/to/existing.py
*** Move to: path/to/renamed.py
@@ optional context line (for example a function or class signature)
 unchanged context line
-removed line
+added line
*** End of File
*** End Patch
```

Rules:

- Paths are relative to the working directory unless absolute.
- `*** Add File:` lines are prefixed with `+`; every added line becomes part of the file.
- `*** Delete File:` removes the file; it is an error if the file does not exist.
- `*** Update File:` is followed by one or more `@@` chunks. A chunk contains context
  lines (prefix ` `), removed lines (prefix `-`) and added lines (prefix `+`).
- `*** Move to:` may follow an `*** Update File:` header; the updated content is written
  to the new path and the original path is removed.
- `*** End of File` marks the end of the current chunk as anchored to the end of the file.
- Change lines before the first `@@` are allowed; an implicit chunk is opened for them.
- A chunk may contain only `@@` (or only `*** End of File`) to insert at a position
  without matching existing text.
- To delete a line, include it with a `-` prefix and do not add a replacement.
- Keep at least one context line around each change when it helps locate the hunk.
- All hunks are validated against the current files before anything is written. If any
  hunk cannot be applied, no file is modified. Re-read the affected files and regenerate
  the entire patch instead of resending the same patch.
