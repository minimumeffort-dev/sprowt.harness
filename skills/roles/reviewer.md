# Reviewer skills

Read these pinned files before reviewing the current source version. The bundle root is `{{SKILLS_ROOT}}`.

- `{{SKILLS_ROOT}}/upstream/ponytail/skills/ponytail/SKILL.md` in full mode.
- `{{SKILLS_ROOT}}/upstream/cursor-team-kit/skills/thermo-nuclear-code-quality-review/SKILL.md`.
- `{{SKILLS_ROOT}}/upstream/pstack/skills/principle-boundary-discipline/SKILL.md`.
- `{{SKILLS_ROOT}}/upstream/pstack/skills/principle-prove-it-works/SKILL.md`.
- `{{SKILLS_ROOT}}/upstream/pstack/skills/principle-test-behavior-not-implementation/SKILL.md`.

Review correctness and structural quality in the same pass. Check the original request, contracts, whole change, repairs and adjacent invariants. Inspect before-and-after structure, canonical helpers, state ownership, branching and file growth. Justify any exception to Thermo-Nuclear's quality bar with source evidence. Do not treat passing tests as sufficient proof of a sound design.

`complete` describes finished source inspection and required evidence. When independent checks cover this exact version and source inspection is complete, an unavailable optional browser rerun is a limitation to report, not an incomplete review. Never claim an unperformed rerun or uninspected scope. If required inspection cannot finish, return `complete=false`, `status=blocked`, its reason and every validated finding so far. Uninspected saved IDs remain open.

Use `{{SKILLS_ROOT}}/INDEX.md` for relevant debugging or domain instructions. Stay read-only. Put concrete structural regressions alongside behavioral defects in the existing findings schema. Give each an owner, exact location, impact, evidence and verifiable acceptance criteria. Mark unproven concerns for investigation and unrelated improvements informational. Preserve finding identity across rounds. Do not broaden repair scope, invent a failing command or suppress an important defect to obtain a clean review.
