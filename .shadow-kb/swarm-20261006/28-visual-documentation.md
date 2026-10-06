# Assignment 28: DAF visual documentation

## Done
Added committed assets/swarm.svg and docs/SWARM.md. README links preview, reusable specialist plans, the executable Rust example and DJcode integration. Mermaid describes review, validation, execution and evidence.

## Found
Standalone specialist missions execute sequentially; the local CLI scheduler and bundled DJcode host have different contracts. Outputs do not automatically flow through the specialist-plan API.

## Decisions
Use concise source documentation, explicit boundaries and authored SVGs. Credit Darshankumar Joshi without generated-by footers.

## Tests and evidence
The registered-worker specialist_plan example ran successfully; its reporting workers demonstrate routing, not model inference. Independent code-to-doc review requested before publication.

## Open questions and limits
No external-effect exactly-once or general-intelligence guarantee is made.

## Next steps
Primary: finish combined acceptance, review current diff and publish source PRs. No Actions or managed release was started by this assignment.
