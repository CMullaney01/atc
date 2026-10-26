# The skills this repo is written under

atc's Rust is not written to whoever's default taste is loaded that day. It is
written to a fixed set of rules kept as skill files, so the code reads the same
whichever machine or assistant touched it last.

This page is the part that has to survive a fresh clone: **what exists, and when
to load it.** The rules themselves live in the skill files.

## Where they are, and why they are not here

```
.claude/skills/
├── INDEX.md          the full catalog, with a row per skill
├── rex-code-philosophy/SKILL.md
└── …
```

`.claude/` is excluded by a global gitignore, so **a fresh clone has none of
this** — only the pointer in `CLAUDE.md` and this page. They come from the
[`rex-skills`](https://crates.io/crates/rex-skills) crate (MIT). The same
command installs and refreshes them:

```sh
cargo install rex-skills && rex skills
```

That also brings the `rex` helpers: `rex codebase --rust-only --with-context`
prints the module map, and `rex smells` lists every `SMELL:` flag left in the
code.

## Loading them

A skill is a file. Loading one means reading it **in full** before starting —
through the `Skill` tool where that exists, or by reading its `SKILL.md`.

Three load before any Rust in this repo:

| Skill | What it governs |
|---|---|
| `rex-code-philosophy` | design decisions — complexity-averse, ergonomics first |
| `rex-code-ergonomics` | idiomatic Rust: naming, types, traits, iterators, signatures |
| `rex-code-commenting` | the best comment is none; WHY only, never WHAT |

**"Every time" means every batch of Rust, not once per sitting.** Loading them
and then writing a new module an hour later without reloading is a real failure
mode, not a hypothetical one: `src/doctor.rs` was written that way and broke
three rules at once — a `Result<T, String>` the error rules forbid outright,
missing derives, and file reading welded to the logic so the part that mattered
could not be tested on its own. All three were caught afterwards by an audit
that should not have had to find them.

## The rest, by when you need them

| Load this | When |
|---|---|
| `rex-code-tdd` | writing code — it is *how* code gets written here |
| `rex-code-error-writing` | anything that can fail. No `Result<T, String>`, no `unwrap` outside tests |
| `rex-code-external-libraries` | adding or upgrading a crate |
| `rex-code-folder-management` | creating, splitting or moving a module. 500 lines a file |
| `rex-code-improve-codebase-architecture` | refactors and architecture reviews |
| `rex-code-smells` | auditing. Finds problems, never fixes them — the fixes live in the skills above |
| `rex-code-tests-unit-testing` | inline tests, and deciding which to delete later |
| `rex-code-tests-contract-seams` | a module boundary changes |
| `rex-code-tests-integration-testing` | code touches the file system, sockets, or time |
| `rex-code-tests-fitness-functions` | an invariant must never break |

Not about code: `rex-grill-me` stress-tests a plan, `rex-mermaid-diagrams`
draws, `rex-skill-language` is for writing skills, `rex-caveman` is terse mode,
and `rex-codex-adversarial-review` has a second model challenge the work.

`rex-code-lead` and `rex-code-orchestrator` run the whole pipeline across
several agents. Neither is needed for ordinary changes here.

## What this actually buys

The rules are only worth keeping while they are enforced, and the enforcement is
`cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`,
and a smell audit before anything merges. `.pre-commit-config.yaml` is committed
but inert — its hooks want `pre-commit`, `gitleaks` and `trivy`, none of which
are installed. To turn it on:

```sh
brew install pre-commit gitleaks trivy
pre-commit install --hook-type pre-commit --hook-type pre-push
```
