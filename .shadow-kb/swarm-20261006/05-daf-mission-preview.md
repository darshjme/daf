# Assignment 05: DAF mission CLI preview

## Done
Added --dry-run to validate the entire local mission and display dependency waves, exact argv and working directories as text or JSON. Preview returns before confirmation or spawning commands. --dry-run conflicts with --yes.

## Found
Read the existing graph validation and execution path before adding preview. Dependency waves expose scheduling readiness, not completion or model decisions.

## Decisions
Reuse existing validation and preserve explicit execution authorization.

## Tests and evidence
16 real CLI checks passed before the final plain-output regression was added; final combined CLI acceptance belongs to assignment25.

## Open questions and limits
No preview executes commands. A valid preview does not predict command success.

## Next steps
Primary: finish combined acceptance, review current diff and publish source PRs. No Actions or managed release was started by this assignment.
