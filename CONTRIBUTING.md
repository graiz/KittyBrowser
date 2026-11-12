# Contributing

Thanks for helping. kb is a small project with one maintainer, so this page
says what makes a contribution easy to accept.

## Reporting a bug

Open an issue with:

- your OS, terminal and its version, and browser (`kb --version`, and the
  Chrome version);
- what you did, what you expected, and what happened;
- for display or input problems, a log: run `KB_LOG=/tmp/kb.log kb URL`,
  reproduce it, and attach the file.

Security problems go privately instead; see [SECURITY.md](SECURITY.md).

## Suggesting a change

For anything bigger than a fix (a new command, a new dependency, a change of
behaviour) open an issue first and describe the problem it solves. It is
much easier to agree on the problem before the code exists, and some ideas
are out of scope (see "Not goals" in [docs/ROADMAP.md](docs/ROADMAP.md)).

## Pull requests

A pull request is likely to be merged quickly when it:

- **does one thing**, explained in the description: what problem, what
  changed, and how you checked it;
- **passes CI**: `cargo test`, `cargo fmt --check` and `cargo clippy
  --all-targets -- -D warnings`, on macOS and Linux;
- **has tests** for logic that can be tested without Chrome (parsing, URLs,
  command arguments), and says what was checked by hand in a running kb;
- **updates the docs** it affects: README.md for users, `skill/SKILL.md` for
  agents, docs/ARCHITECTURE.md for design decisions, and CHANGELOG.md under
  "Unreleased";
- **matches the code around it** and adds no dependency without a reason
  (see [AGENTS.md](AGENTS.md), which applies to people too).

It is likely to be declined, or to need a discussion first, when it adds a
dependency the standard library could replace, adds an async runtime, opens
kb to the network, works on one platform at the expense of the other, or
reformats or refactors code it doesn't otherwise need to touch.

**Review:** expect a first response within a week. The maintainer may push
small fixes to your branch, and merges with a squash, so the commit message
is the pull request's title and description.

**AI-assisted contributions** are welcome. You are responsible for the
change as if you wrote it: read it, run it, and say in the description which
parts an agent wrote.

**Commits:** short imperative subject line (under about 70 characters), then
a body that says why.

## Releases

Versions follow [semantic versioning](https://semver.org); before 1.0, a
minor version may change behaviour. Each release gets a CHANGELOG.md entry
and a git tag (`v0.2.0`).

## Conduct

Everyone taking part is expected to follow the
[code of conduct](CODE_OF_CONDUCT.md).

By contributing, you agree that your contribution is licensed under the MIT
license, as the rest of the project is.
