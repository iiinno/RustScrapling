# RustScrapling — instructions for coding agents

Read **`Plan.md` first**. It is the acceptance contract, not an inspirational
roadmap. The pinned Python upstream is `D4Vinci/Scrapling` at
`95cf9019ab5a5d6f3e9654b7ef1c6a26e29dc5c0` (Scrapling v0.5).
`HANDOFF.md` describes the current integration state and unmerged branches.

- Target: functionally complete Rust reimplementation of Scrapling v0.5,
  including Selector/Adaptive, HTTP, Browser, Spider, CLI, daemon and MCP.
  Unimplemented options must return typed errors, never fake success.
- Do not declare features, test suites, parity, performance or release gates
  complete without evidence from the exact integrated SHA. See `G-01`–`G-13`
  and stage acceptance IDs in `Plan.md`.
- Work in a **separate Git branch/worktree**. Inspect `git status`, `git log`,
  unmerged changes and other active agents before editing or merging. Avoid
  overwriting anybody's local uncommitted files.
- **Never run Cargo/Rust builds on the existing Windows PC or WSL.** Use
  supported Linux CI or the VPS in an isolated worktree with `systemd-run
  --user --scope`, bounded memory, `nice`, `timeout` and low Cargo job count.
  Protect running services: never stop all services, run global `killall`,
  alter live deployment configs or use another agent's worktree as scratch.
- Prefer deterministic localhost tests and the pinned Python oracle. Live
  Chrome E2E must be separate from mocked tests. Distinguish `ignored`,
  `filtered`, `blocked`, `timeout` and actual `PASS`.
- Keep cookies, proxy credentials, API tokens, Chrome profiles and private
  data out of Git, fixtures and tool output; constrain network and IPC access.
- Respect the upstream BSD-3-Clause and referenced Rust MIT/Apache-2.0
  licenses. Record attribution in `NOTICE` / `licenses/` when reusing code.
- For each change report: branch/commit SHA, files, commands, exit codes,
  evidence, remaining acceptance IDs, known risks and next step. Prefer a
  reviewable PR against `main` rather than direct production changes.
