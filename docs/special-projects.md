# Special-project execution

Strategic selection still uses the existing superweapon policy, including the
nuke target-value threshold, alliance turn-taking and existing allied frames.
The special-project service retries placement every ten seconds independently
of workers asking for jobs. It surveys deterministic rear sites for silos,
Zeniths and Raveparties; Berthas retain their firing-site evaluation.

TaskStore owns accepted construction. PendingPower owns power dependencies.
Logistics owns staffing and progress: special projects use its shared resource
allocation, yield to energy emergencies, and retain task identity while paused.
Their former unconditional thirty-second unstarted-task expiry is removed.
Unsafe or invalid unstarted sites still fail normal validation. Existing projects
finish before a changed strategic selection creates another project. Operator
project deadlines remain authoritative.

The economy endpoint's `special_project` and journal `special` events report
placement blockers, power waits, accepted tasks, staffing, pauses and completion.
`issue` events provide crew and progress details. A strategic request is no longer
logged as construction before the task has been accepted.

This is per-bot allocation. Allied partially built projects can replace our own
proposal, and shared nuke/turn rules avoid duplication; the service does not
reserve another team's workers or income.

The checkpoint-enabled tree persists retry deadlines, status, strategic choice,
tasks, pending power and coordinator state. Tests cover a non-default coordinator
round trip and the next worker assignment under unchanged policy. Adding the
status field changes the strict snapshot schema; older saves are rejected. The
current lobby branch does not yet include the separate checkpoint implementation.
