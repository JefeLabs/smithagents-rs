# Feedback

The **Feedback** entry on the rail, just below Settings, collects pictures of the SmithAgents window and files, with your comments, into one draft. Nothing leaves this machine.

## Capturing the window

- **Click** Feedback (or press ⌘⌥F) to open the Feedback pane; click again to close it.
- **Hold** Feedback for 3 seconds, until its ring is full, to take a picture of the SmithAgents window. Letting go sooner counts as a click; moving off the button cancels. **Capture** in the pane takes a picture without the hold.
- The picture opens over the whole window. **Drag** a box over what you mean and type a comment: **Enter** saves it, **Shift-Enter** starts a new line, **Esc** drops the box (so does saving an empty comment).
- Click a box's number to **edit** its comment: Enter saves, Esc keeps the old one, **Delete** removes the box and the later boxes are renumbered.
- **Done** returns to the pane; **Discard** removes the screenshot (it asks first when it has boxes). Click a screenshot's card to annotate it again.
- A screenshot holds at most 20 boxes, each at least 8×8 points, with a comment of at most 500 characters.

## Adding files

**Add files…** accepts png, jpg, gif and webp images of at most 10 MB each and mp4 videos of at most 50 MB. A file's contents must match its extension. A draft holds at most 10 attachments and 100 MB in all. Each refused file is named in the pane with the reason; the other files are still added.

## The draft

The draft lives in `<home>/feedback/draft/` (`<home>` is `~/.smithagents`, or `SMITHAGENTS_HOME` when set) and survives quitting the app. **Discard** in the pane deletes it, after asking. Nothing is sent anywhere: **Submit…** is shown but disabled, and submitting arrives with the next update.

## Screen Recording permission (macOS)

The first capture asks macOS for Screen Recording permission. Allow SmithAgents in **System Settings → Privacy & Security → Screen Recording**, then quit and reopen SmithAgents: macOS reports the permission only after a restart. Without it, **Add files…** still works.

## Manual check

Real capture has never been run by the automated tests: they use a fake capturer, and no test opens the file dialog. After a build, run `cargo run -p host-desktop` with `SMITHAGENTS_HOME` set to a scratch directory, and check:

1. The first capture shows the system's permission prompt, and capture works after allowing it and reopening the app.
2. A real capture of the app's own window works on the primary display and on a second display.
3. The captured image is not gray or empty (the capture library uses a macOS call Apple has deprecated).
4. The 3-second hold and its ring feel right.
5. Writing a large screenshot does not stutter when the annotator opens.
6. The disabled **Submit…** button shows its tooltip.
7. The annotator can be used from the keyboard.
8. A Linux build needs libclang and the PipeWire development packages installed; it has not been tried.
