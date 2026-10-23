- In both owner and claimed mode, a task tagged `no-diff-expected` runs
  no workspace gates or required validation commands and attaches no
  `no-diff.json`, `already-landed.json`, or workspace validation logs.
  Run only commands its own brief requires: a QA sweep may build and run
  the product, and a review may reproduce a finding when its brief asks
  for it. Record that work in the brief's evidence artifacts and the
  execution summary. Leave the worktree clean; the pipeline hands off
  on the tag. This exception applies throughout items 7, 10, and 12.
