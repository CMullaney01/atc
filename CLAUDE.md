# atc

Rust. The skills in `.claude/skills/` govern how this project is written — read
them, don't re-derive the rules. `.claude/skills/INDEX.md` is the catalog.

Load these three before writing any code here, every time:

- `rex-code-philosophy` — complexity-averse, ergonomics-first; governs design decisions
- `rex-code-ergonomics` — idiomatic Rust: naming, types, traits, iterators, panics, signatures
- `rex-code-commenting` — the best comment is none; WHY only, never WHAT

Then load whatever the task calls for from the index — `rex-code-tdd` for writing
code, `rex-code-error-writing` for anything fallible, `rex-code-external-libraries`
before adding a crate, `rex-code-smells` for an audit.

## Getting the skills onto a machine

They are not in the repo — `.claude/` is globally gitignored, so a fresh clone
has no `.claude/skills/`. Install them from the
[`rex-skills`](https://crates.io/crates/rex-skills) crate (MIT), which is also
how to refresh them:

```sh
cargo install rex-skills && rex skills
```

That also wants to write `.pre-commit-config.yaml` and install git hooks needing
`pre-commit`, `gitleaks` and `trivy` on PATH. Not set up here — `cargo fmt`,
`cargo clippy -D warnings` and `cargo test` are run by hand instead.

The crate's `rex codebase` and `rex smells` helpers need that `cargo install`;
without it, `grep -rn 'SMELL:' src/` does the same job.
