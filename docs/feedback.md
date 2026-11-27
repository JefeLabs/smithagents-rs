# Feedback

The **Feedback** entry on the rail, just below Settings, collects pictures of the SmithAgents window and files, with your comments, into one draft. **Submit…** commits the draft to one of your repositories as an issue folder and turns it into an intent, a GitHub issue or an idea package. No screenshot or file leaves this machine before you submit; opening the Submit screen only asks `gh` whether it is signed in (`gh auth status`) and, for each repository whose `origin` is on GitHub, whether that repository is public (one `gh repo view` each). After Submit, the daemon pushes only for **A GitHub issue** and **Open pull request**, and an intent's agent may open a pull request of its own (see [what is published](#what-is-published)).

Contents: [capturing](#capturing-the-window), [adding files](#adding-files), [the draft](#the-draft), [what submitting needs](#what-submitting-needs), [submitting](#submitting), [what is committed](#what-is-committed-and-where), [what is published](#what-is-published), [Retry and Start over](#retry-and-start-over), [idea packages](#idea-packages), [limits](#limits), [not built / known gaps](#not-built--known-gaps).

## Capturing the window

- **Click** Feedback (or press ⌘⌥F) to open the Feedback pane; click again to close it.
- **Hold** Feedback for 3 seconds, until its ring is full, to take a picture of the SmithAgents window. Letting go sooner counts as a click; moving off the button cancels. **Capture** in the pane takes a picture without the hold.
- The picture opens over the whole window. **Drag** a box over what you mean and type a comment: **Enter** saves it, **Shift-Enter** starts a new line, **Esc** drops the box (so does saving an empty comment).
- Click a box's number to **edit** its comment: Enter saves, Esc keeps the old one, **Delete** removes the box and the later boxes are renumbered.
- **Done** returns to the pane; **Discard** removes the screenshot (it asks first when it has boxes). Click a screenshot's card to annotate it again.
- A screenshot holds at most 20 boxes, each at least 8×8 points, with a comment of at most 500 characters.

## Adding files

**Add files…** accepts png, jpg, gif and webp images of at most 10 MB each and mp4 videos of at most 50 MB. A file's contents must match its extension. A draft holds at most 10 attachments and 100 MB in all. Each refused file is named in the pane with the reason; the other files are still added. Once the draft is full, the rest of the pick is not looked at and one sentence counts those files.

## The draft

The draft lives in `<home>/feedback/draft/` (`<home>` is `~/.smithagents`, or `SMITHAGENTS_HOME` when set) and survives quitting the app. **Discard** in the pane deletes it, after asking; while a submission of it may still be in progress, the question adds "A submission of this draft may still be in progress. Discarding deletes the screenshots here; anything already sent stays." A successful submission deletes it too. Nothing in it is sent anywhere before you submit; once you have, a submission whose later step failed may already have pushed its files (see [Retry](#retry-and-start-over)), although the draft is still here.

A draft that cannot be read at start (damaged, or saved by a newer version) is set aside as `<home>/feedback/draft.broken-<time>/` and a new, empty draft begins. That folder holds the old draft's screenshots and files: delete it when you no longer need it. Only the newest set-aside draft is kept. If the draft cannot even be set aside, the pane says it cannot be saved, and nothing is written over it.

## What submitting needs

| For | You need |
| --- | --- |
| Every submission | A source repository added in Settings → Source repositories, whose default branch exists locally. git 2.36 or newer. A git identity (`user.name` and `user.email`) that applies to that repository. |
| **A GitHub issue** and **Open pull request** | `origin` on github.com (an `https://`, `ssh://` or `git@github.com:` address; another host or a remote with another name does not count). The GitHub CLI `gh`, installed and signed in to github.com: `gh auth status --hostname=github.com` must succeed. git able to push to `origin` with your own credentials and without asking you anything. |
| **Demo video** in an idea package | `agent-browser` installed. |

A plain intent and **Create implementation intent…** need nothing more, also for a repository whose `origin` is on GitHub: without `gh`, or signed out, that repository counts as possibly public, and the app asks before sending (see [what is published](#what-is-published)).

The daemon supplies no credential: git pushes with your own setup (a credential helper, an SSH key). A credential git would have to ask for fails the push at once; over SSH, a prompt (an unknown host key, a passphrase) makes the push wait until its 120-second limit.

Without `gh` or `agent-browser`, only the choices that need them are unavailable; capture, files and **An intent to work on now** work without either. The tools belong to the built-in `feedback` plugin: it shows `needs_setup` naming a missing tool, and installs one on request through the daemon's API (see [the feedback plugin](plugins.md#the-feedback-plugin); there is no screen for it in the app yet). Turning that plugin off turns both tools off.

## Submitting

**Submit…** is enabled once the draft has a screenshot or a file ("Add a screenshot or a file first" otherwise). It opens the Submit screen in the Feedback pane; **Back** or **Esc** returns to the draft.

- **Main idea** (required): the first line is the title (at most 120 characters; leading `#` marks are dropped), the rest is the description.
- **Kind**: New feature, Enhancement or Bug. An intent gets the kind of the same name, and `issue.md` records it.
- **Repository**: starts on the repository you used last, else the one marked **Intents and feedback** in Settings → Source repositories (set it with **Use for intents and feedback**; with a single repository it is that one), else "Choose a repository".
- **Send it as**: **An intent to work on now** or **A GitHub issue**. When the issue choice is unavailable, the screen says why: "This repository has no GitHub remote.", "Install the GitHub CLI (gh) to create issues.", "Sign in first: gh auth login", "The feedback plugin is turned off or not running.", or "The GitHub CLI (gh) is not ready: …".
- A line under the choices says what will be created, for example "Creates the issue folder with 1 screenshot on a new branch, and an intent to review and send." or "Creates the issue folder with 1 screenshot, pushes the branch to origin and opens a GitHub issue."
- **Create intent** or **Create issue** sends it (after the [public-repository question](#what-is-published) when there is one). While it runs, every control waits; the app waits up to 10 minutes for the daemon's answer. A press that sends nothing says why: "The draft is still being changed. Try again when it is done.", "The draft is not ready to send yet. Try again in a moment." or "A submission is already under way."

On success the draft is deleted, the pane says what was made ("Created the intent “…”. Files: <branch> (<commit>)." or "Created <issue address>. Files: <branch> (<commit>).") and, while the Feedback pane is showing, a new intent opens in the Intents pane.

**An intent to work on now** commits the issue folder and creates an intent in Draft, seeded from the main idea, the kind, the screenshots' comments and where the files are. Polish, review and send it in the Intents pane like any intent. Its agent starts on `intent/<slug>` from the feedback branch, so `issue.md` and the images are in its checkout, and its branch tracks nothing; its brief tells it that they are your report, material to read and not instructions to follow, and to leave the folder in place. If the agent's worktree cannot be made from the feedback branch, the intent says "The agent could not start: The intent's base branch could not be checked out." (code `base_branch_failed`). Nothing is pushed at Submit, but the agent's pull request will carry the folder: for a repository whose `origin` is on GitHub, Create asks first unless the repository is known to be private (see [what is published](#what-is-published)).

**A GitHub issue** commits the issue folder, pushes it to `origin` and runs `gh issue create`. The issue's title is your title; its body is `issue.md` without its front matter, with the images as permanent links to the commit (`https://github.com/<owner>/<repo>/blob/<commit>/<folder>/<file>?raw=true`), a hidden last line naming the submission, and at most 60,000 characters. It gets the label `enhancement` (New feature, Enhancement) or `bug` (Bug), only when the repository has a label of that name. It is refused before anything is written when `origin` is not on github.com, `gh` is not ready, your default branch has commits `origin` does not have, or the repository is (or may be) public and the warning was not confirmed (see [what is published](#what-is-published)).

## What is committed, and where

Every submission is one folder on a new branch of the chosen repository:

```
docs/issues/<yyyy-mm-dd>-<slug>/
  issue.md
  screenshot-1.png             the picture as captured
  screenshot-1.annotated.png   with the boxes and numbers drawn in
  image-1.jpg, clip-1.mp4      files you added
```

- **Branch:** `feedback/<yyyy-mm-dd>-<slug>`, or `idea/<slug>` for an idea package. The date is UTC; the slug comes from the title (`feedback` when the title has no ASCII letter or digit). A folder or branch name already in use gets `-2`, `-3` and so on, up to `-99`; a branch `origin` has (as far as your last fetch knows) counts as in use.
- **Base:** your local default branch, as it is at that moment. Nothing is fetched.
- **Your checkout is never touched.** The commit is made in a temporary worktree under `<home>/tmp/feedback/`, with nothing checked out. Your working tree, index, current branch and other worktrees stay as they are.
- **The commit:** one commit, `docs(issues): <title>`, ending with the line `Feedback-Id: <submission id>`. Its author is your git identity. It is not signed, and no hook runs.
- **Ordinary blobs:** the files are stored byte for byte. The repository's attributes and filters (Git LFS, git-crypt, `working-tree-encoding`, `ident`, line-ending conversion) are not applied to them. In a repository that keeps images in LFS, these images are in plain git history, and a server that accepts such files only through LFS rejects the push.
- **`issue.md`:** a small front matter (title, kind, created, app version, platform), the title as a heading, the rest of the main idea, then one section per attachment: a screenshot's annotated picture with its numbered comments and a link to the original, an image, or a link to a video. Your words are kept, but made safe for a Markdown file: the title is one line (control characters, line and paragraph separators and bidirectional overrides become spaces, leading `#` marks are dropped, at most 120 characters) and is Markdown-escaped in the heading; the rest of the main idea keeps its lines as typed, without control characters other than line breaks and tabs and without those separators and overrides; each comment becomes one line cleaned the same way, at most 500 characters (never cut by bytes, whatever the script), and Markdown-escaped; an added file's name is shown without its folders, cleaned and escaped. Mentions are left as typed in the file.
- The tool never deletes these branches.

Refused with nothing made: a default branch that does not exist locally ("the repository's default branch main does not exist locally"), a file where the folders go ("docs/issues is a file in this repository, so feedback cannot be stored there."), a branch named `feedback` or `idea` ("a branch named feedback exists in this repository, so no branch can be created under feedback/"), no git identity ("git has no user.name and user.email set for this repository"), and a repository git cannot read, because its folder was moved or deleted or is no longer a git repository ("git could not read this repository's settings. Check that its folder is still a git repository.", code `origin_unreadable`; checked before the others, whatever the submission is sent as). For an issue submission already recorded, Retry reports that sentence as its failed step instead, and **Open pull request** and **Create implementation intent** refuse with it before anything is pushed or made.

## What is published

No screenshot or file leaves this machine before Submit (opening the Submit screen runs `gh auth status` and one `gh repo view` per GitHub repository, which send nothing of the draft). The daemon pushes only for **A GitHub issue** (at Submit or Retry) and **Open pull request**, only to `origin` of the repository shown, never forced, and always through the push guard below. What it pushes is a recorded commit itself, whatever the local branch points at by then. Replacement objects and grafts made with `git replace` are ignored, so what is checked is what is pushed.

**The push guard.** The folder commit is made on top of your local default branch, so pushing it would also publish every commit of yours that `origin` does not have. Before an issue submission commits anything (or takes up a commit a stopped daemon left), and again inside every push (Submit, Retry, Open pull request), the daemon checks, from what git last fetched and without using the network, that everything under the folder commit is already on `origin` (`refs/remotes/origin/*`; another remote's branches do not count). If it is not, it refuses with code `unpushed_commits` and one of:

- "The default branch has commits that are not on origin. Push them first, so they are not published with this feedback."
- "This repository has never been fetched from origin, so it is not known what is already published. Run git fetch first."

Refused before the commit, nothing is made and the form stays editable. Refused at a push, nothing is pushed: push your commits (or fetch), then Retry or press **Open pull request** again. An idea package to a GitHub repository is checked before its commit too, once the repository has been fetched from `origin` (a repository never fetched is let through, and asked to fetch at **Open pull request**), so the agent's work is not wasted on a pull request that would be refused; its **Open pull request** is checked like any push. A plain intent is not checked by the guard. The check reads your remote-tracking branches, so after a push made elsewhere, `git fetch` first.

**Public repositories.** Screenshots can show chat text and file paths. With **A GitHub issue** chosen, the Submit screen says "This repository is public. The screenshots will be visible to anyone." (or "This repository may be public. The screenshots will be visible to anyone." when `gh` cannot tell). With **An intent to work on now** (no idea package) for a repository whose `origin` is on GitHub, it says "This repository is public. The agent's pull request will include the screenshots, visible to anyone." (or "This repository may be public. The agent's pull request will include the screenshots, visible to anyone."). Before every send to such a repository, Retry included, it asks "Send the screenshots to GitHub?"; Cancel sends nothing. A repository known to be private, or without a GitHub remote, is not asked about. An idea package is not asked at Submit: nothing of it is published before its two actions, which ask. **Open pull request…** always asks first: "Pushes the idea branch to origin and opens a draft pull request. If the repository is public, the screenshots will be visible to anyone." **Create implementation intent…** always asks first too ("Create an implementation intent?"): "Creates an intent to build this idea. Its agent's pull request will include the idea's screenshots and explainer. If the repository is public, they will be visible to anyone." In each question Cancel has the focus and Enter cancels.

The daemon enforces this itself. A repository that is public, or whose visibility `gh` cannot tell (also when `gh` is not installed or not signed in), counts as public: an issue submission, a plain intent submission to a repository whose `origin` is on GitHub, **Open pull request** and **Create implementation intent** are refused (code `public_not_acknowledged`, nothing committed, pushed or created) unless the request says the warning was confirmed, with "This repository is public. The screenshots will be visible to anyone. Confirm that before sending." or "This repository may be public. The screenshots will be visible to anyone. Confirm that before sending." A confirmed request is not asked of `gh` at all, so an intent works without `gh`. The app says so only after its question was confirmed for that send. A commit a stopped daemon left is asked about as a new submission before it is taken up. Retry asks the question too, but a retry of a recorded submission sends no confirmation, since the daemon does not ask again (see below); only when the daemon has no record of the id and the app sends the draft again does that send carry the answer. If the daemon refuses a submission although the app took the repository for private, the app asks the same question (with the "may be public" warning of the issue or of the intent) and, on confirm, sends again under the same id. While the repository list has not loaded (it is still loading, or could not be read), **Create** can send only an intent and does not ask first: the daemon's refusal brings the question. On Cancel nothing is sent. For a first send the form then stays editable; a refused re-upload (the files Retry sends again when the daemon has no record of the id, see [Retry](#retry-and-start-over)) leaves the draft locked, and the button is **Retry**. That question opens only while the Feedback pane is showing: otherwise the refusal's sentence stays on the Submit screen, and the next **Create** (for a re-upload, the next **Retry**) meets it again. A submission is checked once, before its commit: a recorded submission that is continued or retried is not asked again. **Open pull request** is checked on every press, **Create implementation intent** on every press until it is made. Whether a repository is public is asked of `gh` and remembered for 10 minutes, for each repository on its own.

**Mentions.** In what is sent to GitHub (an issue's title and body, a pull request's title and body), every `@` followed by an ASCII letter or digit gets an invisible zero-width space after it, so nobody is notified (`@name`, `(@name)`, `.@name`, `-@name`, `/@name`, `@@name`, `@org/team`), unless the character before it is an ASCII letter or digit or `_`: `a@b.com` and `photo@2x.png` stay as typed. A body is shown as Markdown, where a web address GitHub turns into a link mentions no one, so there an `@` inside such an address is left alone too, but only where GitHub's own rules start and end the link: the address starts at the start of the text, after ASCII white space (a space, a tab, a line break), or after `*`, `_`, `~` or `(`; it begins with `http://` or `https://`; its host is ASCII letters, digits, `-` and `.`, begins and ends with a letter or digit and holds a `.`; then maybe `:` and a port, then `/`, `?` or `#`; and it goes on only while each character is an ASCII letter or digit or one of `- . _ ~ / % ? # = & + : @`. So `https://www.npmjs.com/package/@types/node` and `[docs](https://a.b/@scope/pkg)` stay as typed, while in `[x](https://a.b/y)@name` the `@name` after the link gets the invisible character. The rule is narrower than what GitHub links, so some typed addresses still get the invisible character: one right after `[`, `<`, a quote or `=` (`<https://a.b/@name>`), one with `,`, `;`, `!`, `*`, a bracket or a letter that is not ASCII in its path before the `@`, one on `localhost` (`http://localhost:5173/@vite/client`), and one without `http://` or `https://` (`www.example.com/@name`). A title is not shown as Markdown, so an address in it gets the character like any other text. The committed files keep your text unchanged.

**An idea agent's commits** are published as the agent wrote them, messages and authors included, when you open the pull request. Before pushing, the daemon checks that every commit on top of the idea's own changes only files inside the issue folder; otherwise nothing is pushed.

**An intent's agent.** Once you send a feedback intent, its agent is told, as every intent's agent is, to open a pull request of its work when it is done (when the repository has a GitHub remote). That pull request carries the issue folder with its screenshots, which is why a plain intent to a public (or possibly public) GitHub repository is asked about at Submit, and an implementation intent at **Create implementation intent…**. The agent pushes with its own `git push`; the daemon's push guard does not cover it.

## Retry and Start over

A draft keeps one submission id for every send and Retry, and the daemon makes at most one commit, one intent and one issue per id: sending the same id again, or twice at once, continues what it recorded instead of starting again.

When a step after the commit fails (the intent, the push or the issue), the pane says "The files are committed on <branch> (<commit>), but: <sentence>" and offers **Retry** and **Start over**. Retry runs only what has not succeeded: the folder is never committed again, a push sends the same commit, and an issue is looked for before one is created.

**When the draft locks.** The draft (text, kind, repository, attachments) locks while something may exist under its id: a step failed after the commit; no answer came within 10 minutes; the connection broke after the request left; the daemon answered with a server error or a code the app does not know. The app marks the draft as sent before the request leaves, so after a quit, a closed window or a lost connection it comes back locked, with Retry, and the Submit screen says "This feedback may already have been sent. Press Retry to find out, or Start over to send it as a new submission." (also when an answer came that the app cannot take for this submission's: one about another submission, success or failed step, or one that names neither an intent nor an issue address it shows; nothing of it is kept or shown, and nothing is deleted). While it is locked, nothing can be changed or added: "This feedback was sent or is being sent. Retry or start over in Submit before changing it." Retry asks the daemon to continue first, and sends the files again only when the daemon has no record of the id. That second upload is made only because an earlier request under the id went unanswered, which may still make its commit: if the daemon refuses it, the draft stays locked (fix the cause and Retry, or Start over).

**When it stays editable.** The draft stays editable, under the same id, when the request never left (not connected, the connection refused, a file that could not be read) and when the daemon refused it before making anything (`invalid`, `unsupported_file`, `payload_too_large`, `no_github_remote`, `gh_not_ready`, `origin_unreadable`, `tool_missing`, `default_branch_missing`, `git_identity_missing`, `no_free_name`, `name_taken`, `folder_blocked`, `branch_blocked`, `unpushed_commits`, `public_not_acknowledged`). The refusal's sentence stays on the Submit screen until the next send. A repository that no longer exists says "That repository no longer exists." and the id is dropped. When the daemon has no record of a submission whose failed step the draft keeps (after a reset of its data, say), the id is dropped too, and the line names what is still committed: "The daemon has no record of this submission. Its files are still committed on <branch> (<commit>). Submit again to send it as a new one." (without such a step, "Nothing was created. Submit again.").

**Start over** drops the pending submission and unlocks the form. The screenshots, the text, the repository, the choice and the idea boxes stay as they were, and the next **Create** sends them as a new submission, with a new id; what was already committed stays on its branch. When no answer came, it asks first: "The daemon may still be finishing this submission. Starting over can create it twice." **Discard** works whenever a submission is pending.

**Duplicates.**

- A submission keeps running in the daemon when the app quits, and the daemon does not stop for idleness while one runs. There is no way to cancel it.
- After `gh issue create` ran without a usable answer, Retry creates nothing for a minute: "GitHub has not confirmed whether the issue was created. Try again in a minute."
- After the daemon restarts, a second issue is prevented only by a search of your own issues (`--author=@me`) for the submission's hidden line. GitHub's search can lag: an issue created seconds before may not be found, and a second one is then created.
- If the daemon stopped between the commit and its record, sending the same draft again takes up that commit (found by its `Feedback-Id` line) instead of committing again, but only in the same repository and only while that branch still points at the commit; it is checked first as a new submission would be (the push guard, the public-repository question). Otherwise a second commit is made, on a branch of its own.
- If the daemon made the intent but could not record it, Retry finds that intent (by the folder, branch and commit it was made from) instead of making a second one.

## Idea packages

Under **An intent to work on now**, tick **Prepare an idea PR for teammates** and choose its parts: **Demo video**, **Diagrams**, **Explainer doc**. Ticking it ticks **Explainer doc**; at least one part is needed ("Choose at least one part."). The boxes start unticked each time the Submit screen opens. The line under the choices then reads, for example, "Creates the issue folder with 1 screenshot. An agent adds an explainer doc and a demo video on an idea branch, and you open the pull request."

The feedback becomes an idea intent, committed on `idea/<slug>`, that you polish, review and send in the Intents pane like any intent. Its agent prepares the package in the issue folder and does not implement the idea, push, or open a pull request; polish asks what the package should cover, never how to implement it. Each part is an instruction to the agent and a fixed acceptance criterion:

| Part | The agent is told | Fixed criterion |
| --- | --- | --- |
| **Explainer doc** | Write `explainer.md` with the sections "What", "Why", "How it could work" and "Open questions". | `explainer.md` exists in the issue folder and covers what, why, how it could work and open questions. |
| **Diagrams** | Add diagrams to `explainer.md` as Mermaid code blocks; if a Mermaid renderer is installed, also save one rendered image per diagram in the folder and link it. | `explainer.md` has at least one Mermaid diagram of the idea. |
| **Demo video** | Run the project, reproduce the bug or walk through the idea in a browser with `agent-browser`, save the recording as `demo.mp4` (at most 50 MB) and the steps taken in `demo.md`. Offered only while `agent-browser` is installed ("Only works for projects a browser can show."); otherwise disabled with "Install agent-browser to record a demo video." | `demo.mp4` (at most 50 MB) shows the bug or the idea in a browser, and `demo.md` lists the steps taken. |
| Always | If a part cannot be produced, say why under a "Not done" heading in `explainer.md` and go on. Commit only files inside the folder, then stop. | Only files inside the issue folder are changed, and nothing is implemented. |

With **Explainer doc** unticked there is no prose explainer: with **Diagrams**, the agent writes only a minimal `explainer.md` that holds the diagrams and, if a part is not done, the "Not done" list; without **Diagrams**, it creates no `explainer.md` unless a part is not done, and then puts only the "Not done" list in it.

The fixed criteria are accepted from the start and cannot be edited or dropped: the detail tags each with "fixed" and offers no Edit or Skip on it, the review routes refuse ("this item is fixed: it cannot be edited or dropped"), and polish keeps them first and unchanged. An idea intent's agent is never told to rebase onto its dependencies (that would rewrite the idea's commit). An intent made from feedback cannot move to another repository ("This intent's evidence is in its repository, so the repository cannot be changed."), nor can one with a fixed dependency ("This intent depends on another intent in its repository, so the repository cannot be changed.").

**Open pull request…** appears in the idea intent's detail once the intent is sent and its agent is done, or once it is resolved. After you confirm, the daemon (on a public repository, only with that confirmation: see [what is published](#what-is-published)):

1. takes the agent's branch (`intent/<slug>`) as it is, and refuses with nothing pushed or recorded when a commit on it changes a file outside the issue folder ("the idea agent changed files outside the issue folder, so nothing was pushed"), when it does not contain the idea's commit, when it has more than 1000 commits on top of it, when one of those commits holds a Git LFS pointer under the folder, at the tip or in an earlier commit, since the push publishes the whole history even when a later commit replaced the pointer ("The idea's files were committed through Git LFS, now or in an earlier commit of the agent's branch, which this push does not upload. Turn Git LFS off for docs/issues and have the branch's commits made again without it.", code `idea_lfs`; checked again at every press until the push), when `issue.md` is gone ("The idea's issue.md is missing from the agent's branch, so a pull request cannot be opened."), or when the push guard refuses;
2. only then records that commit, and pushes it to `origin` as `idea/<slug>`, never forced (the push checks the guard again);
3. opens a draft pull request into the default branch with `gh pr create --draft`: the intent's title, and a body of `issue.md` and `explainer.md` with the images as permanent links. A pull request you already opened from that branch for that same commit is used instead; one whose head is another commit is never taken. If one of yours from that branch is open with another head (the branch changed on GitHub after the push), none is opened beside it and nothing is recorded: "A pull request for this idea's branch is already open on GitHub, but the branch there has changed since it was pushed. Open that pull request on GitHub." (code `pr_head_moved`). Carry on in that pull request; once it is closed there, the next press opens a new one. A closed or merged one for another commit changes nothing.

The detail then shows the pull request's address, which opens in the browser on a click (not a right click), with a copy button beside it (its tooltip is "Copy link") that puts the address on the clipboard, and the agent's "Not done" section, if `explainer.md` has one ("Not done: …"). If the push or `gh` fails, press **Open pull request…** again: what succeeded is not repeated, and the same commit is pushed while the agent's branch still holds it (as its tip or in its history). A commit the branch no longer holds (you amended or reset it) is never pushed: the branch is read again and checked as at a first press. If an earlier press had already pushed the commit you amended away (its record of the push was lost, say), `origin` refuses the new one, since nothing is forced: "git could not push to origin: origin already has that branch with other commits, perhaps from an earlier push. Delete the branch on origin if it is no longer wanted, then try again." (code `push_not_fast_forward`; an issue's push answers the same when `origin` has a branch of its name with other commits). The earlier commit stays published under `idea/<slug>` on `origin` until that branch is deleted there; then press again. After `gh pr create` ran without a usable answer, nothing is opened for a minute: "GitHub has not confirmed whether the pull request was opened. Try again in a minute." The app waits up to 10 minutes for either action's answer. A refusal or failure shows the daemon's sentence, first letter capitalized, on one line and cut at 300 characters ("The agent is still working", "The idea intent has not been sent yet", the sentences above), except for the codes the app has a sentence of its own for: "That intent no longer exists." (`intent_not_found`), "That repository no longer exists." (`repo_not_found`), and its intent commands' sentences for `polishing`, `already_answered`, `dependency_cycle` and `dependency_cross_repo`. (`invalid_state` shows the daemon's sentence here, not the intent commands' "The intent changed, so this is no longer possible.") An answer that is not the daemon's (a proxy's page) shows "The daemon could not do that."; no connection, a broken one or no answer in time show the app's own lines.

**Create implementation intent…** asks first (see [what is published](#what-is-published)), then makes, once, the intent that implements the idea: same kind and repository, starting from the idea agent's branch (so the package is in its checkout), depending on the idea intent (a fixed dependency), seeded with "Implement the idea “<title>”." and where the package is. It opens in the Intents pane while the idea's detail is still showing; pressing again opens the same intent. Nothing is pushed, and since the app sends the confirmation, `gh` does not run.

## Limits

| What | Limit |
| --- | --- |
| Attachments per submission | 10 |
| One image, screenshot, or annotated screenshot | 10 MiB (10 × 1024 × 1024 bytes) |
| One video | 50 MiB |
| All files of a submission, annotated screenshots included | 100 MiB |
| Boxes per screenshot | 20 |
| A comment | 500 characters |
| The main idea | 20,000 characters |
| The title (the first line) | 120 characters; the rest is cut |
| An issue's body on GitHub | 60,000 characters |
| Folders of one name on one day | 99 |
| `demo.mp4` | 50 MB, as the agent is told; the daemon does not check it |
| Commits on the idea agent's branch, for a pull request | 1000 |
| A push | 120 seconds |
| One `gh` call; the sign-in and visibility checks | 60 seconds; 5 seconds |
| The app waiting for the daemon's answer (Submit, Retry, Open pull request, Create implementation intent) | 10 minutes |

Files are recognised by their first bytes, not by their names: PNG, JPEG, GIF, WebP and MP4; a screenshot and its annotated picture must be PNG. A video is recognised by the `ftyp` box every ISO media file starts with, so MOV and other ISO media files sent as video are accepted too, and stored as `clip-<n>.mp4`.

## Screen Recording permission (macOS)

The first capture asks macOS for Screen Recording permission. Allow SmithAgents in **System Settings → Privacy & Security → Screen Recording**, then quit and reopen SmithAgents: macOS reports the permission only after a restart. Without it, **Add files…** still works. Started from a terminal (`cargo run`), the app's permission is the terminal's: macOS asks for, and lists, the terminal app, not SmithAgents.

## Building on Linux

A Linux build of the workspace needs libclang and the PipeWire development packages, which the capture library uses; on Debian or Ubuntu its own list is `pkg-config libclang-dev libxcb1-dev libxrandr-dev libdbus-1-dev libpipewire-0.3-dev libwayland-dev libegl-dev`. Capture on Linux and Windows has never been built or run.

## Not built / known gaps

- **Capture on Linux and Windows** has never been built or run, and no test covers it.
- **No cancel.** Once a submission's upload or push has started, it cannot be stopped from the app; the daemon finishes it.
- **The system git configuration applies.** The daemon's git reads the system-wide configuration, which the push needs for a system credential helper. A system `url.<base>.insteadOf` there can also send the push to another address.
- **The push guard knows only the last fetch.** It reads your remote-tracking branches and never uses the network. If `origin` was force-pushed, or a branch was deleted there, and you have not fetched since, the commits your remote-tracking branches still name count as already on `origin`: the guard passes, and the push publishes those commits again.
- **"Already on origin" is judged from the fetch address.** The remote-tracking branches come from `origin`'s fetch address, but the push goes to its push address (`remote.origin.pushurl`, or a `pushInsteadOf` rule). When the two differ, commits that the fetch address has and the push address has not are published with the feedback.
- **An idea's agent can publish a commit of yours.** On the idea path, a local commit that `origin` does not have and that changes only files inside the issue folder can be merged by the idea intent's agent: the scope check lets it through, and **Open pull request** publishes it with its own message and author.
- **A "private" answer is trusted for 10 minutes.** Whether a repository is public is remembered for 10 minutes; a repository made public within that time is sent to without the question.
- **A retry goes where `origin` points now.** A recorded submission that has not been pushed yet is pushed, at Retry, to wherever `origin` points at that moment, and is not asked about again.
- **A crashed run can leave a worktree registration.** If the daemon crashed while committing, the repository can keep a locked registration of the temporary worktree (`git worktree list` shows it) until the next submission to that repository removes it.
- **Idea packages in repositories that track media with Git LFS.** The agent commits with your git, so `demo.mp4` or a rendered diagram may become an LFS pointer; **Open pull request** then refuses (`idea_lfs`) until Git LFS is turned off for `docs/issues` and the branch's commits are made again without it: a later commit that replaces the pointer is not enough, since the earlier one would be pushed with it.
- **Open owner decisions:**
  - whether commits that will be pushed should be built on `origin`'s default branch instead of your local one (today the push guard refuses when your local branch is ahead);
  - whether **Open pull request** should publish one commit the daemon makes instead of the agent's own commits, which are published as the agent wrote them.

## Manual check before release (for testers)

Real capture has never been run by the automated tests: they use a fake capturer, and no test opens the file dialog. Nor has a real `gh`, `agent-browser` or push to GitHub: the tests use fake tools and local repositories. After a build, run `cargo run -p host-desktop` with `SMITHAGENTS_HOME` set to a scratch directory (macOS then asks Screen Recording permission for the terminal, not SmithAgents), and check:

1. The first capture shows the system's permission prompt, and capture works after allowing it and reopening the app.
2. A real capture of the app's own window works on the primary display and on a second display.
3. The captured image is not gray or empty (the capture library uses a macOS call Apple has deprecated).
4. The 3-second hold and its ring feel right.
5. Writing a large screenshot does not stutter when the annotator opens.
6. On an empty draft, **Submit…** is disabled and shows its tooltip.
7. The annotator can be used from the keyboard.
8. A Linux build, with the packages under **Building on Linux**, compiles; it has not been tried.

With a throwaway private GitHub repository of your own:

9. The `feedback` plugin shows `needs_setup` naming a missing tool, and `ready` once `gh` and `agent-browser` are installed.
10. Submitting as an intent creates the branch and opens the intent, and the working tree is untouched.
11. Submitting as an issue pushes with your own git credentials; the issue shows its images and has the right label.
12. With the network off, the push fails with a plain sentence, and Retry works once it is back.
13. An idea package: the agent writes `explainer.md` and, when `agent-browser` really records, `demo.mp4`; **Open pull request** opens a draft. Record what `agent-browser` actually produced: the recording has never been run.
14. The public-repository warning shows for a public repository, for an issue and for a plain intent, and **Create implementation intent…** asks first.
