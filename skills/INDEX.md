# Load only what the task needs

Paths are relative to this bundle. Read [POLICY.md](POLICY.md) first. Each role names its required starting files in [roles](roles/README.md).

| Work | Read |
| --- | --- |
| Any coding change | [Ponytail, full mode](upstream/ponytail/skills/ponytail/SKILL.md) |
| Split work and define checks | [Verifiable units](upstream/pstack/skills/principle-sequence-verifiable-units/SKILL.md) |
| Design state or contracts | [Domain model](upstream/pstack/skills/principle-model-the-domain/SKILL.md), [boundaries](upstream/pstack/skills/principle-boundary-discipline/SKILL.md) |
| Implement a feature | [Feature workflow](upstream/pstack/skills/poteto-mode/playbooks/feature.md) |
| Debug or repair | [Bug fix workflow](upstream/pstack/skills/poteto-mode/playbooks/bug-fix.md), [root causes](upstream/pstack/skills/principle-fix-root-causes/SKILL.md) |
| Refactor | [Refactoring workflow](upstream/pstack/skills/poteto-mode/playbooks/refactoring.md) |
| Verify work | [Prove it works](upstream/pstack/skills/principle-prove-it-works/SKILL.md), [test behavior](upstream/pstack/skills/principle-test-behavior-not-implementation/SKILL.md) |
| Retries, concurrent work or shared state | [Idempotency](upstream/pstack/skills/principle-make-operations-idempotent/SKILL.md), [separate shared state](upstream/pstack/skills/principle-separate-before-serializing-shared-state/SKILL.md) |
| Check implementation quality | [Thermo-Nuclear review](upstream/cursor-team-kit/skills/thermo-nuclear-code-quality-review/SKILL.md), [reader load](upstream/pstack/skills/principle-minimize-reader-load/SKILL.md) |
| Write docs, commit or PR text | [Technical writing](upstream/pstack/skills/technical-writing/SKILL.md), [unslop](upstream/pstack/skills/unslop/SKILL.md) |

The complete pstack plugin is retained for its local references, playbooks and provenance. Its presence does not activate every workflow. Explore its [skill directory](upstream/pstack/skills) for a relevant instruction, then read only that file and its needed references. The Sprowt policy defines how to handle external tools and skills that are unavailable.
