This task changes something a user sees, so check its visual evidence (the
orbit skill's task-review reference). List the task's artifacts with
`orbit.task.show` (`field: "artifacts"`), fetch the screenshots with
`orbit.task.artifact.get` and look at them against the criteria: each changed
view, in every theme it supports, and at 375px when the layout is responsive.
Missing screenshots without a stated reason, such as no browser kit on the
host, are missing evidence: record a spec-compliance finding.
