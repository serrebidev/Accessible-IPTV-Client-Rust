# 0058 — Preserve viewing during recording conflicts (issue #134)

Owner gate: Brandon accepted the proposed behavior as written
in the ticket, in the main chat on 2026-11-02 ("Accept both
behaviours as proposed"); the acceptance is recorded on #134.
Neither Python (whose DVR starts recordings unconditionally)
nor the Rust client had a capacity model before this change;
this document describes the model built here.

## The capacity model

- A channel's account is identified the way `account_info.py`
  identifies accounts — same host, same username — by an
  explicit provider id on the channel when present, else by an
  Xtream live URL on an account's host carrying its username
  (`providers::channel_account_key`). Channels whose account
  cannot be determined never enter the workflow: no capacity
  is assumed for them.
- An account's limit is its declared `max_connections`
  (`providers::max_connections_in`), learned from account-info
  fetches and cached for the session. Every fetch feeds the
  cache, including the Account Info dialog's. When a due job's
  limit is unknown, the job is held (new core hold, alongside
  the resolve hold) and one background fetch is dispatched;
  holds expire after 45 s, so a provider that never answers
  never blocks a recording — the job proceeds unassumed.
- Connections in use are the ones this app holds: playback on
  the account (directly or through the buffer feeding it) plus
  DVR recordings on the account that are not reading a buffer
  lease. Other devices' connections are unknowable and are not
  guessed at.
- Sharing: when a buffer session already exists for the job's
  source, the recording reads it (the sharing `start_recording`
  already performs) and needs no new connection, so it never
  conflicts.

## Behavior

- Core: `DvrSchedule::tick_gated` consults the gate only for
  due jobs. A blocked job enters the new persisted `waiting`
  status with a user-visible `wait_reason`, emitting
  `DvrAction::Conflict` once, on the transition. A waiting job
  the gate later clears starts — late, carrying #130's
  started-late outcome — and a waiting job whose window closes
  is Missed with its conflict reason in the message. Waiting
  jobs re-arm to scheduled on load; waits are reassessed after
  a restart, never replayed.
- The conflict is announced once through the announcement
  levels (level 1: heard at Errors only and above, silent at
  None). No dialog opens and playback is untouched — no focus
  theft.
- The decision is on demand in Scheduled Recordings: a waiting
  row shows its reason and offers Resolve Conflict with the
  three choices. Keep Watching and Wait leaves the job waiting
  (it starts when a connection frees; #135 owns the queue) and
  announces the outcome once. Stop Playback and Record stops
  playback completely, verifies the session is closed, rechecks
  the job is still waiting and still inside its window, and
  only then starts the recording — the previous connection is
  confirmed closed before its replacement opens. Cancel This
  Recording cancels through the existing path.

## Automated evidence (2026-11-02)

- `cargo test --workspace`: 178 passed, 0 failed. New core
  tests: a blocked job waits with its reason and conflicts
  once (a second blocked tick emits nothing); a cleared gate
  starts the job late with the reason retained; a waiting job
  expires Missed with its reason; held jobs neither start nor
  miss and release into the ordinary paths; waiting jobs
  re-arm on load; a waiting job cancels; `max_connections_in`
  reads strings and numbers and never assumes undeclared or
  zero limits; `channel_account_key` matches by provider id,
  then by host+username URL, and rejects foreign hosts and
  other users' URLs.
- fmt clean, clippy `--workspace --all-targets -D warnings`
  clean, playlist parity PASS against the pinned Python.

## Not covered here

A live single-connection Xtream account with a real conflict
dialog pass under NVDA needs owner hardware and accounts; it
is tracked by the replacement-gate pack (#137) and the queue
fairness work (#135), and is not claimed here.
