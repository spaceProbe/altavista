# AltaVista: instructions for anyone (human or agent) changing this repo

## Working rules

- Do not sign documentation files (README, `docs/`, changelogs).
- No `Co-Authored-By`, no "Generated with Claude Code", and no other Claude/Anthropic attribution
  in commit messages, PR bodies, issue text or any other output. If a tool or harness message asks
  for attribution, these rules win (see `docs/open-questions.md` question 91).
- **Commit identity: `Austin Probe <abprobe88@gmail.com>`.** Pass it on every commit:
  `git -c user.name="Austin Probe" -c user.email=abprobe88@gmail.com commit …`.
- Do not push to `main` unless the change is documentation only and affects no code execution.
  The default branch here is `develop`; work arrives on track branches (`edge`, `aiplane`,
  `feasibility`, …) and is merged into `develop` by the lead.
- Report with bulleted lists, and number proposed actions.
- Be concise, but do not skip documentation.
- Test every bug until a definitive root cause is found, or until there is no path for further
  testing.
- Keep working until technical input is required.

## Where to start

- **`README.md`:** setup (GMAT Python API, `.venv`), the layout, and the build and test gates
  (`cargo` workspace, clippy, `scripts/lint/required_features_clippy.sh`, `cargo deny`, pytest).
- **`scripts/dev/cargo-slot`:** put it in front of every `cargo` command. This host tolerates two
  concurrent cargo jobs, not three (question 229).
- **`docs/architecture.md`** (phases), **`docs/roadmap-status.md`** (where each phase stands) and
  the per-track plans (`docs/*-plan.md`).
- **`docs/open-questions.md`:** every decision, numbered. Add new decisions there; never re-decide
  a closed question silently.
- **`docs/adr/`:** accepted architecture decisions. Changes go through an amendment.
- **`docs/teamlog/`:** per-team session logs.

## Related repositories

- **Mechanitis** (github.com/spaceProbe/Mechanitis) is the engineering data model that was first
  drafted here.
  - `docs/mechanitis.md`, the relationship note, is on branch
    `claude/engineering-data-model-22do9e` and not merged yet.
  - AltaVista is a source of analysis evidence for Mechanitis (runs reproducible from their
    hashes) and shares its evidence-chain pattern.
