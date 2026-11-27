# Feedback Tool Design

Status: built (`docs/superpowers/plans/2026-11-26-feedback-capture.md`,
`docs/superpowers/plans/2026-11-26-feedback-submit.md`); where the build
differs, see [Deviations from this design](#deviations-from-this-design).
User guide: `docs/feedback.md`. Marks: [owner] is a decision the owner
made on 2026-11-26; [my call] is mine and open to change.

Parent spec: `docs/superpowers/specs/2026-11-23-smithagents-architecture-design.md`.

## Intent

While using SmithAgents, the owner sees something that should change. They
want to point at it, say what they mean, and turn that into work in under a
minute: either an intent an agent can start on now, or a GitHub issue for
whoever picks it up. The evidence (annotated screenshots, extra images, short
videos) travels with it as files in a repository, so an agent or a teammate
reads the same thing the owner saw.

## Goals

- One gesture captures the app window and opens an annotator. [owner]
- Feedback can hold several screenshots, each with numbered comments, plus
  files added by hand (images and mp4). [owner]
- Submitting produces files under `docs/issues/` in a repository the owner
  picks, and then either an intent or a GitHub issue. [owner]
- Capture works on macOS, Windows and Linux, and SmithAgents provides it
  itself and does not depend on an operating-system command. [owner]
- The owner can ask an agent to prepare an idea package for teammates: any of
  an explainer doc, diagrams, and a recorded demo video of the bug or idea
  made with `agent-browser`. [owner]
- The feedback tool is a built-in plugin with command-line requirements
  (`gh`, `agent-browser`), so missing tools show up and install the way the
  GitHub plugin's `gh` does. [owner]

## Non-goals

- Recording video inside the tool. mp4 files are attached by hand. [my call]
- Free-hand drawing, arrows, text labels, blur or crop. Annotation is boxes
  with comments. [my call]
- Recording a demo of SmithAgents' own window. An agent's demo video is
  recorded with `agent-browser`, which drives a web page; SmithAgents' window
  is native. [my call]
- Capturing other applications or a whole screen.
- An intent editor. The intent UI is Plan 3b; this tool only creates the
  intent and says where it is.

## Anti-goals

- Nothing leaves the machine before Submit. Capture, annotate and Done only
  touch local files in the app's own data directory.
- A push to a remote happens only for "GitHub issue", only on Submit, and
  only to the repository shown in the dialog.
- No token or secret is written into the markdown, the commit or the issue.
- Feedback is never lost silently: a failed submit keeps the draft.

## What the owner sees

1. **Rail.** A Feedback entry sits on the icon rail directly under Settings.
   [owner] A click opens the Feedback pane in the right dock. Holding the
   entry for 3 seconds captures the window and opens the annotator; [owner]
   a ring fills around the icon while holding, and releasing early is a
   plain click. [my call] The pane also has a "Capture" button, so the hold
   is a shortcut and not the only way in. [my call]
2. **Annotator.** The captured image fills the window with a bar on top:
   "Screenshot n of m · Drag a box over what you mean", Done, Discard.
   Dragging draws a box; releasing opens a small text field anchored to the
   box ("Enter saves · Esc drops", a delete button). Saved boxes show a
   numbered badge at their top-left corner. Clicking a badge reopens its
   comment. Done returns to the Feedback pane.
3. **Feedback pane** (right dock, title "FEEDBACK"). A collapsed "How to
   submit feedback" help row; one card per attachment: a thumbnail, "Screenshot
   n · k comments", the comments listed with their badges, a remove button.
   Buttons: "Add files…" (png, jpg, gif, webp, mp4), "Capture", Discard,
   "Submit…". The draft survives closing the pane and restarting the app.
4. **Submit screen.** A screen of the Feedback pane, not a modal dialog
   ("Back" returns to the draft). Thumbnails and comments (read-only); **Main idea**
   (multi-line; the first line is the title; required); **Kind**: New
   feature, Enhancement, Bug; **Repository**: a picker over the Space's
   source repositories, starting on the last one used, or the Space's meta
   repository when none has been used; [owner] **Send it as**: "An intent
   to work on now" or "A GitHub issue". A line under the choice says exactly
   what will be created. Buttons: Back, and "Create intent" or "Create
   issue".
5. **After submit.** A status line in the pane: the files' branch and
   commit, and either the intent's title or the issue's URL. The draft is
   cleared. On failure the Submit screen shows the daemon's message and the
   draft stays.

## Idea package ("Prepare an idea PR for teammates")

Under "An intent to work on now" the dialog has a checkbox **Prepare an idea
PR for teammates** with three more: **Demo video**, **Diagrams**,
**Explainer doc**. [owner] The line under the choice then reads, for example,
"Creates the issue folder with 1 screenshot. An agent adds an explainer doc
and a demo video on idea/<slug>, and you open the pull request."

- **It goes through the intent tool, not around it.** [owner, 2026-11-26]
  With the box ticked, the feedback becomes an intent like any other, and the
  requested parts become that intent's acceptance criteria ("explainer.md
  exists and covers what, why, how and open questions", "demo.mp4 shows the
  bug", and so on). It is polished, reviewed and sent from the intent tool,
  and the agent, worktree, queue and status are the intent's own. The
  feedback tool starts no agent itself. This needs the intent tool (roadmap
  Plan 3b) to exist first.
- The branch is named `idea/<slug>`, and the intent's brief carries these
  fixed instructions: read `issue.md` and
  its images; write `explainer.md` (what, why, how it could work, open
  questions); for Diagrams, add diagrams to it as Mermaid blocks plus one
  rendered image each when a renderer is available; for Demo video, run the
  project, reproduce the bug or walk the idea in a browser with
  `agent-browser`, and save `demo.mp4` (at most 50 MB) with the steps taken
  in `demo.md`; commit everything in the issue folder; change nothing else;
  then stop. [brief wording mine]
- **The pull request is for the idea only.** The idea intent's agent
  prepares the package and stops; it does not implement. When the intent
  resolves, it offers **Open pull request**, which pushes the branch and
  runs `gh pr create` (draft, body from `issue.md` and `explainer.md`).
  Implementation is a second intent that depends on the idea intent and
  starts from its branch. [my call; the owner was asked and has not
  answered] The agent works on `intent/<slug>` branched from `idea/<slug>`,
  and "Open pull request" pushes that work to `origin` as `idea/<slug>`.
- **Demo video** is offered only when `agent-browser` is ready and is
  disabled, with the reason, when it is not. It is for projects a browser
  can show. For feedback about SmithAgents itself the box is disabled.
- If the agent cannot produce a requested part (the project will not start,
  no page shows the bug), it says so in `explainer.md` under "Not done" and
  the pane repeats that line. A missing part never fails the whole package.

## A built-in plugin with tool requirements [owner]

The tool appears in the plugin list as a built-in plugin with id `feedback`.
Its manifest declares no resources to sync and two tools:

- `gh` (GitHub CLI): needed for "A GitHub issue" and "Open pull request".
- `agent-browser`: needed for Demo video; installable through mise
  (`npm:agent-browser`; the builder confirms the package name and that it
  can record video before relying on it).

This reuses what the plugin system has: the tool check, the `needs_setup`
state with install hints, the install route, and the rule that the daemon
runs tools and strips `SMITHAGENTS_*` from their environment. The plugin
contract today only describes syncing, so the feedback behaviour itself
(the submit route, the commit, starting the agent) is daemon code that
belongs to this plugin id; it is not loaded as a dylib or a wasm component.
A missing tool disables only the options that need it, not the whole tool:
`needs_setup` on this plugin is advice, and capture, annotate, files and the
intent path work without either tool. [my call: this is the smallest way to
make it "a plugin with CLI requirements" without inventing a second plugin
contract; the manifest must be allowed to have zero resources]

## The meta repository

One per Space (one daemon is one Space): the repository that holds the
Space's intents and feedback files. [owner] It is one of the Space's source
repositories, chosen in Settings → Source repositories ("Use for intents and
feedback"). With a single source repository it is that one. Until it is
set, the picker has no default and Submit asks for a repository.

## Files

Each submission is one folder in the chosen repository: [owner, folder
name mine]

```
docs/issues/<yyyy-mm-dd>-<slug>/
  issue.md
  screenshot-1.png            the image as captured
  screenshot-1.annotated.png  boxes and badges drawn in
  clip-1.mp4, image-1.jpg     files added by hand
```

`issue.md` has a small front matter (title, kind, created, app version,
platform) and then the main idea, and one section per attachment embedding
the annotated image with its numbered comments. `<slug>` comes from the
title; a clash gets `-2`, `-3`.

Limits [my call]: 10 attachments; 10 MB per image; 50 MB per video; 100 MB
per submission. Videos over GitHub's 100 MB file limit cannot be pushed, so
the cap sits below it.

## Approaches

**Capture.** Considered: running the operating system's capture command
(macOS only, rejected by the owner because capture must work on three
platforms and belong to us); calling each platform's capture API ourselves
(three implementations and our own `unsafe`); a cross-platform capture
library. Chosen: a new crate `crates/capture` with a small interface of our
own (`capture_window(bounds) -> image`) implemented with the `xcap` library
(0.9.10: macOS, Windows, Linux on X11 and Wayland). [owner: we provide it,
cross-platform; library choice mine] Our code stays free of `unsafe`; the
library can be swapped behind the interface. The app captures its own window
by matching the GPUI window to the library's window list, falling back to the
window's screen rectangle. macOS asks for Screen Recording permission the
first time, and Wayland asks through its portal; if capture is refused the
pane says so and "Add files…" still works.

**Annotator and pane.** Considered: a web page in the center area (blocked
on the open decision about embedding web content); native GPUI. Chosen:
native GPUI. [my call, not objected to]

**Who writes files and talks to GitHub.** Considered: the app itself (puts
git and credentials in the UI process, against the parent spec); the GitHub
plugin (needs the write-proposal system, which has no plan yet); the daemon.
Chosen: the daemon. [my call, not objected to] The owner's click on Submit is
the approval for that one write.

## How it works

- **Draft** lives in the app: `<app data>/feedback/draft/` holds the images
  and a `draft.json` (attachments, boxes as fractions of the image, comments,
  main idea, kind). Annotated images are rendered when Done is pressed.
- **Submit** is one daemon call, `POST /v1/feedback` (multipart: a JSON part
  and the files), behind the bearer token. The daemon:
  1. checks sizes, types (by content, not by file name) and the repository;
  2. creates a branch `feedback/<yyyy-mm-dd>-<slug>` from the repository's
     default branch in a temporary worktree, writes the folder, commits
     ("docs(issues): <title>"), and removes the worktree;
  3. for an intent: creates it through the existing intent service with a
     text seed made of the main idea, the kind and the comments, plus the
     path of the folder and the branch; the kind is passed through;
  4. for a GitHub issue: pushes the branch, then runs
     `gh issue create --title … --body-file …` in the repository. The body is
     `issue.md` with image links rewritten to the pushed branch. Labels:
     `enhancement` for New feature and Enhancement, `bug` for Bug, applied
     only if the label exists;
  5. answers with the branch, commit, folder, and the intent or the issue
     URL.
- `gh` is found and run with the rules the plugin system already has
  (trusted location, no shell, `SMITHAGENTS_*` variables removed, output
  capped). If `gh` is missing or signed out, "GitHub issue" is disabled in
  the dialog with the reason; the intent path still works.
- Failure after the commit (push or `gh` fails): the branch stays, the
  answer says what succeeded and what did not, and the dialog offers
  "Retry" which repeats only the failed step.

## Decisions from the builder check [my calls unless marked]

- **Upload.** `POST /v1/feedback` is multipart with its own body limit of
  100 MB (the daemon's default stays 1 MB). Parts are streamed to a temporary
  directory under the daemon's home, never held whole in memory, and removed
  when the call ends. The daemon may be on another machine later, so the app
  never hands it a local path.
- **Kind.** New feature, Enhancement and Bug map to the intent kinds of the
  same names (the intent tool added `new_feature`). `issue.md` keeps the kind
  the owner chose.
- **Meta repository setting.** A daemon setting (`meta_repo_id`), read and
  written through a settings route like agent defaults, not a flag on each
  repository. Settings → Source repositories shows which one it is and lets
  the owner change it.
- **Repositories without a GitHub remote** can be picked. "GitHub issue" is
  then disabled with the reason; the intent path works.
- **What the dialog needs to know** comes from one route, `GET
  /v1/feedback/targets`: each repository with whether it has a GitHub remote,
  the meta repository, and whether `gh` is found and signed in.
- **Base and remote.** The branch starts from the repository record's
  `default_branch` as it is locally. The push goes to `origin` with git's own
  credential handling; the daemon supplies no credential. If git cannot push,
  the answer says so in git's terms (never its output verbatim) and Retry
  repeats the push.
- **Links in the issue** are permanent links by commit id, not by branch
  name, so renaming the branch does not break them. Feedback branches are not
  cleaned up by this tool.
- **Source of truth** for annotations is `draft.json`. The app draws the
  annotated image from it every time the draft changes and again at Submit;
  the daemon commits what it is sent and does not redraw.
- **The hold.** A new press-and-hold element on the rail (timer and ring).
  When the hold completes, the ring is hidden and the capture is taken on the
  next frame, so the ring is not in the picture. Whatever else is on screen,
  including an open Feedback pane, is captured as it is.

## Left to the builder

- The exact GPUI elements for the hold ring, the annotator and the cards,
  using the existing tokens and the rail's patterns.
- How the app matches its window in the capture library's list.
- The image encoding and thumbnail sizes.
- The route's exact JSON, error codes in the existing `ApiError` style, and
  the client method.
- Where the "last used repository" is remembered (the app's layout store or
  the daemon's settings).

## Suggested split into plans

1. Capture crate, rail entry with hold, annotator, Feedback pane, draft
   storage. Result: annotated screenshots saved locally; Submit is disabled.
   Needs nothing else.
2. **The intent tool (roadmap Plan 3b)**: intents rail entry, list, editor,
   Send. Not part of this design, but it comes before the rest so feedback
   feeds a tool the owner can see and steer.
3. The `feedback` built-in plugin (manifest, tools), meta repository setting,
   `POST /v1/feedback`, files and commit, intent creation that opens the new
   intent in the intent tool, a base branch for Send, the Submit dialog.
4. GitHub issue path: push, `gh issue create`, retry, the public-repository
   warning.
5. Idea package: the idea intent and its criteria, `agent-browser`, "Open
   pull request" on a resolved idea intent.

## Asked of the owner, not yet answered: my picks stand until changed

- **How the agent sees the evidence.** The intent's agent starts from the
  feedback (or idea) branch, so `issue.md` and the images are in its
  checkout and the pull request that fixes the problem carries them. Needs a
  base-branch parameter on Send. (Alternatives: commit to the default
  branch; text only.)
- **Send right away.** After "Create intent" the new intent opens in the
  intent tool, where Send starts the work. The feedback tool has no Send
  button of its own.
- **Public repositories.** Screenshots can show chat text and file paths.
  When the chosen repository is public and the choice pushes (GitHub issue,
  Open pull request), the dialog shows one line: "This repository is public.
  The screenshots will be visible to anyone."
- **Idea pull request is for the idea only** (see Idea package).

## Deviations from this design

Recorded 2026-11-26, when the build was done. The text above is the design
as approved; where the build differs, this list wins. The user's view of the
result is `docs/feedback.md`.

- **The Submit dialog is a screen of the Feedback pane.** "The dialog"
  elsewhere in this design means that screen.
- **"Explainer doc" does something when unticked.** The design says the
  agent always writes `explainer.md`. Unticked, the agent writes no prose
  explainer: with Diagrams, only a minimal `explainer.md` holding the
  diagrams and the "Not done" list; without, no `explainer.md` unless a part
  is not done. Its fixed criterion is set only when the box is ticked; a
  fixed criterion that nothing is implemented is always set.
- **Fixed criteria are enforced.** The review routes refuse to edit or drop
  a fixed item, polish keeps them first and unchanged, and the polish prompts
  are told the intent prepares an idea package. The Intents detail tags a
  fixed item "fixed" and offers no Edit or Skip on it. An intent made from
  feedback, and one with a fixed dependency, cannot change repository
  (`evidence_in_repo`, `fixed_dependency`). An idea intent's agent is never
  told to rebase onto its dependencies (that rewrites the idea's commit).
- **A push guard.** The folder commit is built on the local default branch,
  so a push would publish the owner's unpushed commits too. Before an issue
  submission commits (or takes up a commit a stopped daemon left), before an
  idea package commits to a GitHub repository that has been fetched, and
  inside the one function that pushes (Submit, Retry, Open pull request), the
  daemon refuses (`unpushed_commits`) unless everything under the folder
  commit is on `refs/remotes/origin/*`: "The default branch has commits that
  are not on origin. Push them first, so they are not published with this
  feedback.", or, never fetched, "This repository has never been fetched
  from origin, so it is not known what is already published. Run git fetch
  first." A folder commit that is not in the repository is refused
  (`commit_missing`), and `git replace` objects are ignored, so what is
  checked is what is pushed. A plain intent is not checked by the guard.
  Added by the fix round that keeps unpushed default-branch work from being
  published, after the build described here.
- **The public-repository warning covers every route that publishes, and
  the daemon enforces it.** The design had it in the app only, and for
  issues only. A plain intent publishes too: its agent opens a pull request
  that carries the folder. "A GitHub issue", a plain intent to a repository
  whose `origin` is on GitHub, "Open pull request" and "Create
  implementation intent" (`POST /v1/intents/{id}/implementation` takes the
  same optional body as the pull-request route) are refused (409
  `public_not_acknowledged`) when the repository is public or its visibility
  cannot be determined (also without `gh`), unless the request carries
  `public_acknowledged: true`: "This repository is public. The screenshots
  will be visible to anyone. Confirm that before sending." or "This
  repository may be public. The screenshots will be visible to anyone.
  Confirm that before sending." An acknowledged request runs no `gh`. An
  idea package submission is not asked: nothing of it is published before
  its two actions. The app asks before every such send, Retry included: for
  an intent with "This repository is public. The agent's pull request will
  include the screenshots, visible to anyone." (or "…may be public…"), and
  always before "Create implementation intent…" ("Creates an intent to
  build this idea. Its agent's pull request will include the idea's
  screenshots and explainer. If the repository is public, they will be
  visible to anyone."). A recorded submission that is retried is not asked
  again, so the retry route carries no acknowledgement.
- **Locking per submission.** In the daemon, one submission id runs once at
  a time and a repeat answers with what was recorded; commits are made one at
  a time, and the steps after a commit hold up no other submission. In the
  app, the draft is locked (Retry, Start over) only while something may exist
  under its id; a refusal after which the daemon made nothing keeps it
  editable under the same id, except a refused re-upload (sent only after an
  earlier request went unanswered), which stays locked. An answer that does
  not name this submission and what it made locks too. A locked draft with
  no answer says "This feedback may already have been sent. Press Retry to
  find out, or Start over to send it as a new submission."
- **A crash between the commit and its record** is recovered from the
  commit's `Feedback-Id: <submission id>` trailer: sending the same id again
  takes up that commit instead of making a second one, after the same checks
  as a new submission. An intent made but not recorded is found by its
  origin and taken up.
- **Commits are unsigned and run no hooks; files are ordinary blobs.** No
  repository attribute or filter (Git LFS, git-crypt, encodings) applies to
  the issue folder. git 2.36 or newer is required.
- **Pushes** happen for "A GitHub issue" (at Submit or Retry) and for "Open
  pull request". What is pushed is the recorded commit, never whatever the
  branch points at by then, and never forced. A branch name `origin` has is
  never chosen for a new submission.
- **Mentions are neutralised only in what is sent to GitHub** (titles and
  bodies of issues and pull requests): every `@` followed by an ASCII letter
  or digit gets a zero-width space after it unless an ASCII letter, digit or
  `_` comes before it. In a body (rendered as Markdown) an `@` inside a web
  address GitHub's extended autolink links is left alone, by a rule narrower
  than GitHub's (`http://` or `https://`, a host with a `.`, then `/`, `?` or
  `#`); a title is never a link and keeps the strict rule. The committed
  files keep the owner's text. A comment is capped at 500 characters and
  never cut by bytes.
- **Duplicates.** The search for an issue or a pull request made before
  looks only at the signed-in user's own (`--author=@me`): the submission id
  is public once pushed. After `gh` ran without a usable answer, nothing is
  created for a minute (`issue_unconfirmed`, `pr_unconfirmed`).
- **"Open pull request"** pushes only when every commit of the agent on top
  of the idea's changes only the issue folder (`idea_out_of_scope`), and
  refuses with nothing pushed when the agent's branch has no `issue.md`
  (`idea_incomplete`) or any commit it would publish on top of the idea's
  holds a Git LFS pointer under the folder, at the tip or earlier
  (`idea_lfs`: "The idea's files were committed through Git LFS, now or in
  an earlier commit of the agent's branch, which this push does not upload.
  Turn Git LFS off for docs/issues and have the branch's commits made again
  without it."). The commit to push is recorded only after every refusal,
  and pushed again at a later press only while the agent's branch still
  holds it; a push `origin` refuses because its branch has other commits is
  `push_not_fast_forward`, never forced. A pull request is taken for the
  idea's only when its head is the commit pushed; while one of the branch
  is open with another head, none is opened (`pr_head_moved`). The agent's commits are published as it wrote them.
  "Not done" is read from `explainer.md` when the pull request is opened and
  shown in the idea intent's detail, with the pull request's address and a
  copy button (tooltip "Copy link").
- **Demo video** is offered whenever `agent-browser` is ready, also for
  feedback about SmithAgents itself, with the line "Only works for projects a
  browser can show."
- **The upload** limit is the 100 MB of files plus 1 MiB for the request
  part and the boundaries.
- **The two idea actions** wait for the daemon as long as a submission (10
  minutes), and show the daemon's sentence when it refuses for the intent's
  state.
- **An intent's agent branch tracks nothing** (`--no-track`), and a worktree
  that cannot be made from the intent's base branch is the fixed start error
  "The intent's base branch could not be checked out."
  (`base_branch_failed`).
