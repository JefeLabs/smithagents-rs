# Intents and execution: acceptance results

Plan: `docs/superpowers/plans/2026-11-24-intents-and-execution.md`, Task 12.
Verified on branch `feat/intents-and-web` with the real `smithagentsd` binary
and `crates/server/tests/bin/fake-claude` standing in for `claude`, or with
the service-level suites that use real git worktrees and a fake harness.

| Checklist line | Result | Evidence |
|---|---|---|
| Daemon restart during pass 1 shows Failed with a Retry that works | Pass | Real daemon binary: `crates/client/tests/intents.rs::daemon_crash_during_pass_one_fails_the_intent_and_retry_works` (fake-claude hangs in pass 1, the daemon is SIGKILLed, a new daemon starts, the intent is `Failed{pass:1}`, `retry_polish` reaches `Questions`). Service level: `crates/server/tests/intents.rs::recover_fails_interrupted_polishes`. |
| A chat with 3 messages produces an intent whose seed shows all three | Pass | Real daemon binary: `chat_seed_carries_every_message` (two user turns and their replies, so four messages; the seed text holds every user message and both replies). Service level: `create_from_chat_tags_source_and_defaults_repo` (tool messages are left out by design). |
| Skipping every question still reaches pass 2 | Pass | `crates/server/tests/intents.rs::pass_two_sees_same_repo_candidates_and_skip_marker` and `zero_questions_goes_straight_to_pass_two`; real CLI: `real_claude_polish_smoke` skips every question it was asked and reached `Review` with all items `Pending`. |
| Two intents, one depending on the other: Send both, the dependent shows Queued, Resolve the first, the dependent starts on its own and its agent's first message contains the dependency summary | Pass | `crates/server/tests/queue.rs::resolving_the_dependency_starts_the_dependent` (asserts the first message equals `compose(&x, &[dep]).first_message`, which holds the dependency section); over HTTP: `crates/server/tests/api.rs::full_loop_over_http_ends_with_the_dependent_starting`. |
| Queue limit 1 holds a second ready intent until the first agent is Done | Pass | `crates/server/tests/queue.rs::limit_one_starts_queued_intents_one_at_a_time_in_queued_order` and `a_started_agent_counts_before_its_state_is_recorded`. |
| Send with the repository moved reports an error and leaves the intent Complete | Pass | `crates/server/tests/queue.rs::moved_repository_keeps_the_intent_and_records_the_error` (intent stays `Complete`, `last_start_error` set, no agent row, no worktree directory). |
| Killing the daemon while an intent is Queued and restarting it starts it when ready | Pass (service level) | `crates/server/tests/queue.rs::restart_starts_an_intent_whose_dependency_resolved_while_down` builds a fresh service and scheduler over the same store. Not run as a SIGKILL of the real daemon binary: that needs a multi-intent dependency setup through fake-claude; the crash and restart path of the binary is covered by the pass 1 line above and the scheduler's startup pass is wired in `smithagentsd.rs`. |
| Polish model reaches the CLI | Pass | `crates/server/tests/intents.rs::polish_model_is_passed_to_the_harness` (`StructuredRequest.model`); real daemon: `crates/client/tests/intents.rs::polish_model_env_reaches_the_cli` (`SMITHAGENTS_POLISH_MODEL=haiku` gives `--model haiku` in the CLI argv). |
| Real CLI polish (title, 0 to 5 questions with 2 or more options, pass 2, all items Pending) | Pass | `real_claude_polish_smoke` in `crates/client/tests/e2e.rs`, run once: 1 passed in 29 s. It does not Send. |
| `cargo test --workspace` and clippy clean | Pass | `cargo test --workspace --features smithagents-server/test-util`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all --check`, `cargo themegen check`, `cargo themegen lint`: all clean on the commit that adds this file. |

## Not verified

- Send of a real agent with the real CLI (the smoke test stops before Send on
  purpose; a full agent run is expensive).
- Cost of the real smoke run: the CLI's cost is not surfaced by the daemon
  (costs are not tracked), so none is reported.
