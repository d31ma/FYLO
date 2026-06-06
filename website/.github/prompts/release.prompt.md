---
description: "Bump version, commit, and push to master"
agent: "agent"
tools: [runInTerminal]
---
Bump the version, commit, and push to master.

1. Determine the new version automatically based on unreleased commits:
   `git log $(git describe --tags --abbrev=0 2>/dev/null || git rev-list --max-parents=0 HEAD)..HEAD --oneline`

   Apply these rules to select the bump type:
   - **major** — any commit with a `!` breaking-change marker (e.g. `feat!:`, `fix!:`) or a `BREAKING CHANGE` footer.
   - **minor** — one or more `feat:` commits and no breaking changes.
   - **patch** — only `fix:`, `chore:`, `docs:`, `refactor:`, `test:`, or `perf:` commits.

   Compute the new version by incrementing the corresponding part of the current `"version"` in [package.json](package.json) and resetting lower parts to zero. Show the chosen version and the reasoning to the user before proceeding.

2. Update `"version"` in [package.json](package.json) to the new version.

3. Commit and push to master:
   ```
   git add package.json
   git commit -m "chore: release v<version>"
   git push origin master
   ```
