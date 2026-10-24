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

They are not in the repo — `.claude/` is gitignored, so a fresh clone has no
`.claude/skills/`. Install them from the
[`rex-skills`](https://crates.io/crates/rex-skills) crate (MIT), which is also
how to refresh them:

```sh
cargo install rex-skills && rex skills
```

That gives you the `rex` helpers too: `rex codebase --rust-only --with-context`
for the module map, and `rex smells` to list every `SMELL:` flag left in the
code.

`.pre-commit-config.yaml` is committed but inert — its hooks need `pre-commit`,
`gitleaks` and `trivy` on PATH, and nothing is installed. Until then `cargo fmt`,
`cargo clippy --all-targets -- -D warnings` and `cargo test` are run by hand. To
turn it on: `brew install pre-commit gitleaks trivy` then
`pre-commit install --hook-type pre-commit --hook-type pre-push`.
