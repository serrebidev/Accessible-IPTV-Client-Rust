# Issue tracker: GitHub

Issues and specs for this repository live as GitHub Issues. Use the `gh` CLI for all operations.

## Conventions

- Create an issue: `gh issue create --title "..." --body "..."`.
- Read an issue: `gh issue view <number> --json number,title,body,labels,comments`.
- List issues: `gh issue list --state open --json number,title,body,labels,comments`.
- Comment on an issue: `gh issue comment <number> --body "..."`.
- Apply or remove labels: `gh issue edit <number> --add-label "..."` or `--remove-label "..."`.
- Close an issue: explain the resolution in a comment, then use `gh issue close <number>`.

Infer the repository from the current checkout's GitHub remote. Do not create issues in the upstream `mattpocock/skills` repository.

## Pull requests as a triage surface

PRs as a request surface: no. Triage only GitHub Issues unless this file is deliberately changed later.

## Wayfinding operations

The map is a single GitHub issue labelled `wayfinder:map`; child tickets are linked as sub-issues where supported. Use native GitHub issue dependencies for blocking relationships when available. A ticket is startable only when it has no open blockers and no assignee.

## When a skill says to publish

Publish by creating a GitHub Issue in this repository. Before the first issue creation in a session, verify the target with `gh repo view --json nameWithOwner` from this checkout.
