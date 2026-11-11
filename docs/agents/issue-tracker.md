# Issue tracker: Forgejo

Issues and specs for this repository live as Forgejo issues at https://git.serrebiradio.com/serrebi/Accessible-IPTV-Client-Rust/issues. The GitHub account is suspended and `gh` no longer works. Use the `tea` CLI (login `serrebiradio`) for all operations.

## Conventions

- Create an issue: `tea issues create --title "..." --description "..."`.
- Read an issue: `tea issues <number> --comments`.
- List issues: `tea issues list --state open --fields index,title,labels,body`.
- Comment on an issue: `tea comment <number> "..."`.
- Apply or remove labels: `tea issues edit <number> --add-labels "..."` or `--remove-labels "..."`.
- Close an issue: explain the resolution in a comment, then use `tea issues close <number>`.

Infer the repository from the current checkout's `origin` remote (`serrebi/Accessible-IPTV-Client-Rust` on git.serrebiradio.com). Do not create issues in the upstream `mattpocock/skills` repository.

## Pull requests as a triage surface

PRs as a request surface: no. Triage only Forgejo issues unless this file is deliberately changed later.

## Wayfinding operations

The map is a single Forgejo issue labelled `wayfinder:map`; child tickets link back to it in their description. Use Forgejo issue dependencies for blocking relationships when available. A ticket is startable only when it has no open blockers and no assignee.

## When a skill says to publish

Publish by creating a Forgejo issue in this repository. Before the first issue creation in a session, verify the target with `tea repos serrebi/Accessible-IPTV-Client-Rust` from this checkout.
