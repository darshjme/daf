# Assignment 03: bounded specialist planning

## Done
Added reusable `plan` module exported as `SpecialistPlan`, `SpecialistAssignment`, `PlanLimits`. JSON plan fields use seconds, not Rust Duration object representation. `to_mission(&limits)` validates and composes one assignment into one existing mission phase. `Orchestrator::run_specialist_plan(&plan,&limits)` checks complete registered capability coverage before invoking any worker, then uses real mission execution.

Repaired execution routing: actual mission dispatch now excludes metadata-only agents with no registered WorkerHandler. Added `SpecialistRouter::route_task_for_agents`; standalone dispatch_task retains existing metadata routing behavior. This prevents a ghost eligible agent from winning scoring while a real executable specialist is registered.

## Found
Existing mission execution is sequential across phases and tasks; this is not a concurrent distributed swarm. Phase dependency success gates work, and real WorkerHandler execution exists. Admission, deadlines/retries and cancellation are delegated to the existing mission engine. Outputs are not automatically passed among these WorkerHandler tasks, and these plans do not replace the separate durable remote dependency API.

## Decisions
- Default 32 assignments, 1 MiB serialized plan, one-hour deadline, maximum 3 retries. Configurable limits have absolute supported bounds: 1024 assignments, 16 MiB, 24-hour deadline, 10 retries.
- Each assignment requires 1..32 unique capabilities, named task, explicit positive task deadline within mission deadline, bounded retry policy, unique named dependencies. Reject duplicate/self/missing dependencies and cycles before worker invocation.
- Preflight all assignments avoids doing earlier work before discovering missing specialist capabilities later. Capability coverage is a snapshot, not a reservation; load/shutdown/removal races may still fail execution.
- Agent provisioning, model selection, external side-effect exactly-once, parallel scheduling and intelligence claims are outside this API.

## Tests
Initial full orchestrator unit run: 85 passed, 0 failed. New regressions validate bounds/cycles/missing dependencies/capability/retries/seconds shape; incomplete registered coverage invokes zero handlers; actual specialists execute dependency order despite metadata-only ghost; failed predecessor blocks dependent handler with fail_fast disabled. Final `cargo test -p daf-orchestrator --quiet`: 85 unit tests + 1 doctest passed, zero failed/ignored. `cargo clippy -p daf-orchestrator --all-targets --no-deps -- -D warnings` passed. No repeated audit loops.

## Open questions
No planner/model synthesizes assignments, no automatically inferred credential or capability, no reservation across preflight/execution. Caller remains responsible for plan content and idempotent effects if retries enabled. Existing mission engine does not consume phase concurrency_limit.

## Next steps
Primary: compose validated API with CLI/review workflows as useful; retain honest sequential execution wording. Verify workspace once after merges. No Actions, commits or deployments performed by this assignment.

## Subsequent review correction
Root found the documented absolute upper PlanLimits bounds were missing because an earlier text replacement did not match formatted source. Corrected the actual condition and added regression covering every upper bound plus supported maxima. `cargo test --locked -p daf-orchestrator plan::tests --quiet`:4passed, zero failures; final locked strict all-target Clippy passed. The previous85unit+1doc run preceded this one added regression; combined workspace verification remains primary's responsibility.
