# Planner skills

Read these pinned files before planning. The bundle root is `{{SKILLS_ROOT}}`.

- `{{SKILLS_ROOT}}/upstream/ponytail/skills/ponytail/SKILL.md` in full mode.
- `{{SKILLS_ROOT}}/upstream/pstack/skills/principle-sequence-verifiable-units/SKILL.md`.
- `{{SKILLS_ROOT}}/upstream/pstack/skills/principle-model-the-domain/SKILL.md`.
- `{{SKILLS_ROOT}}/upstream/pstack/skills/principle-boundary-discipline/SKILL.md`.
- `{{SKILLS_ROOT}}/upstream/pstack/skills/principle-prove-it-works/SKILL.md`.

Inspect the existing implementation first. Name the data model, responsibilities and boundaries before splitting work. Give each task a coherent outcome, ownership and evidence that can pass independently. Separate concurrent writes before adding serialization. Keep reproducing and fixing a defect together when the reproduction can pass only after the fix. Place integrated verification after its implementation dependencies.

Consult `{{SKILLS_ROOT}}/INDEX.md` for relevant design or migration guidance. Return the controller's plan schema. Do not implement or start nested agents, setup flows or publication.
