- This task is tagged `skip-validation`. While every path you change
  matches `workflow.light_delivery_paths` (the task context's
  `light_delivery.paths`; a leading `!` excludes), run no local
  validation: no workspace gates from AGENTS.md or CLAUDE.md, no
  `review.baseline_commands`, and no required or final validation
  commands. The pipeline skips required validation for such a candidate,
  hands it off without validation logs and records the skip in the
  task's history; the hosted pull-request checks still gate it. State the
  skip in the execution summary. If the work needs any other path, the
  tag no longer applies: delivery validates the candidate normally, so
  run the checks items 7 and 10 require and say why in the summary. This
  exception applies throughout items 7 and 10.
