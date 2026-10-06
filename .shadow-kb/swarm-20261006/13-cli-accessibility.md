# Assignment 13: DAF CLI output accessibility

## Done
Banner appears only on interactive stderr outside quiet/JSON mode; tracing ANSI requires a terminal and respects NO_COLOR. Preview uses clear dependency waves and task limits.

## Found
Redirected stderr previously included a decorative banner and potentially ANSI sequences.

## Decisions
Preserve machine-readable stdout and readable redirected output.

## Tests and evidence
New real-process redirected-output assertion is included in check_local_cli.py; final result is recorded in25.

## Open questions and limits
This is a text CLI, not an interactive DAF terminal dashboard.

## Next steps
Primary: finish combined acceptance, review current diff and publish source PRs. No Actions or managed release was started by this assignment.
