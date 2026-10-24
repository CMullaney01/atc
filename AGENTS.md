<!-- rex:codebase -->
## Codebase map

Run one of these commands before you search the tree by hand. Each one prints a tree of the working directory and skips the files that git ignores.

- `rex codebase`: every file.
- `rex codebase --rust-only`: only `.rs` files.
- `rex codebase --with-context`: adds the first sentence of each Rust module's `//!` doc comment to its line.
- `rex codebase --rust-only --with-context`: the Rust modules and what each one is for. Start here in a Rust crate.

Add `--for-human` to write the tree to `CODEBASE.md` instead of printing it, for example `rex codebase --for-human --rust-only --with-context`.

## Code smells

Run this command to find each known problem that was flagged and left in the code. It reads every `.rs` file and skips the files that git ignores.

- `rex smells`: one `path:line` for each `SMELL` flag comment in the Rust files, then the count of them all.
