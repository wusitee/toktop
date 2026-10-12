# Repository policy

## Scope

This file is repository-wide Agent configuration, not project documentation. Keep it short, general, and limited to durable rules that apply across tasks and developers.

Do not encode task-specific instructions, temporary decisions, personal preferences, or machine-specific details here. Do not promote a task's instructions or preferences into standing project rules without explicit user authorization.

## Repository hygiene

Project changes intended for Git must be commit-ready: complete, maintainable, validated, and free of temporary or machine-specific content.

Use Git-ignored `.scratch/` for scratch work, drafts, experiments, debugging artifacts, one-off validation, and other non-commit work. When unsure whether an artifact belongs in project history, put it there.

Production code, maintained tests, durable documentation, and build workflows must never depend on `.scratch/`.

## Documentation

Documentation language: **English**. This setting applies only to project documentation, not this policy, source code, identifiers, comments, commit messages, logs, or other implementation text.

Use terminology and phrasing natural to developers working in the specified language. Write concise, factual, technically precise documentation that is clear to both human developers and agents. Preserve necessary context; include only detail that helps readers understand, use, or maintain the project.

Keep durable system documentation in `docs/` and written task briefs, implementation plans, and work logs in `.scratch/`. Document the current system, not the request that caused a change. Remove or update obsolete content; retain history or explanations of removed or rejected work only when needed to understand the current system.

If `README.md` exists, prioritize first-time understanding and getting started. Do not use it as the default destination for documentation changes or as a task record. Keep detailed developer documentation in `docs/`; when present, link to it from a dedicated README section.

Review documentation for impact. Update it when a change makes it inaccurate, exposes missing information necessary for understanding, use, or maintenance, or changes what readers need to know. Leave accurate and sufficient documentation unchanged; restoring already-documented behavior does not by itself require a documentation edit. Make necessary documentation changes with the implementation, in the same commit when committing.

## Local preferences

Put personal preferences and machine-specific settings in ignored, tool-supported local configuration such as `CLAUDE.local.md`, not here.

## Tests

Keep the maintained suite lean and focused on meaningful behavior and established contracts. Reuse or extend suitable existing tests and fixtures; add tests only to close a concrete gap in regression protection. Do not add tests solely because code changed, or assertions that merely reproduce the implementation.

As part of changes to an affected area, proactively simplify its tests and fixtures: consolidate overlapping cases and remove obsolete, redundant, or low-value tests that provide no distinct regression protection. Preserve meaningful coverage and distinct failure modes. Do not sweep unrelated suites or remove, weaken, or skip tests to conceal defects or unresolved failures.

Use `tests/` for maintained tests, respecting existing colocated or framework-required layouts. Maintained tests must be stable, repeatable, maintainable, and provide lasting regression value. Keep one-off tests and reproductions in `.scratch/`; promote them into the maintained suite only when they meet these standards and fill a concrete coverage gap.

## Completion standard

Before finishing, ensure this task's project changes are complete and maintainable, affected documentation is current, and changes intended for Git contain no temporary artifacts or dependencies on `.scratch/`.

Run only required checks: those mandated by applicable instructions or project requirements, plus the minimum targeted validation necessary to verify affected behavior and address concrete risks. Identify project requirements from existing configuration, CI, and contribution guidance. Honor explicitly mandated scope; keep risk-based checks focused on the change.

Reuse still-valid results. Once required checks pass and the affected behavior is adequately verified, stop validation. Repeat or expand it only for a relevant failure, a concrete uncovered risk, or a change that invalidates earlier results. Report unresolved failures and any required checks that could not be completed.

## Policy changes

Do not modify or translate this file unless the user explicitly requests changes to this policy. Documentation language settings and project-wide editing or translation requests do not grant that authorization.

Extend this policy only with durable repository-wide rules that materially improve future work; prefer revising or removing redundant rules over accumulating instructions.
