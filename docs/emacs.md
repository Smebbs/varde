# org-varde

org-varde (the `.el` files at the repository's root) is varde's Emacs
client. Tasks live on the board, notes live in org, and the two meet
through `varde:` links. org-varde holds no state of its own and writes
to org only the links you ask for.

## Install

It needs varde and its hooks ([the guide](guide.md#install)), and
Emacs 29.1 or later. Sessions open in
[vterm](https://github.com/akermu/emacs-libvterm) when it is
installed, else in [ghostel](https://github.com/dakra/ghostel) when it
is, else in `term`. With Doom's workspaces each task gets one.

On Emacs 30 or later:

```elisp
(use-package org-varde
  :vc (:url "https://github.com/Smebbs/varde" :rev :newest)
  :demand t    ; the mode must be on before you mark a heading DONE
  :after org
  :config (org-varde-mode 1)
  :bind (("C-c v b" . org-varde-board)
         ("C-c v i" . org-varde-insert-link)
         ("C-c v t" . org-varde-new-here)
         ("C-c v n" . org-varde-next-needs-you)
         ("C-c v r" . org-varde-recall)
         ("C-c v v" . org-varde-show)))
```

On Emacs 29, install it once with
`M-: (package-vc-install '(org-varde :url "https://github.com/Smebbs/varde"))`
and drop the `:vc` line.

On Doom:

```elisp
;; packages.el
(package! org-varde :recipe (:host github :repo "Smebbs/varde"))

;; config.el
(use-package! org-varde
  :after org
  :config (org-varde-mode 1))

;; SPC v under evil. Without evil Doom's leader is C-c, where v is
;; version control: pick a free letter.
(map! :leader :prefix ("v" . "varde")
      :desc "Board"           "b" #'org-varde-board
      :desc "Link a task"     "i" #'org-varde-insert-link
      :desc "New task here"   "t" #'org-varde-new-here
      :desc "Next needing me" "n" #'org-varde-next-needs-you
      :desc "Recall"          "r" #'org-varde-recall
      :desc "View task"       "v" #'org-varde-show)
```

`org-varde-use-worktree` (`ask`, `always` or `never`) says whether a
task's first run gets a worktree. `M-x org-varde-doctor` checks the
setup.

## In org

**Links.** `org-varde-insert-link` picks a task by its title, with
completion, and inserts `[[varde:k2f0g3][Fix the login bug]]`; a
project's cairn is `varde:project:~/src/app`. A link shows where its
task stands after itself (`● working`, `○ open`, `✓ done`) and follows
it live while `org-varde-mode` is on. Following it opens the task's
view. On the board or in a view, `y` copies a link to the task.

**A task from a note.** `org-varde-new-here` on a heading, or a region,
opens the new-task form, filled in from it:

```
Title:   Fix the SSO redirect
Project: ~/src/app
When:
── context ──────────────────────────────
It loops after the session expires.

From: [[file:~/notes/auth.org::*Fix the SSO redirect][auth.org › Auth › Fix the SSO redirect]]
```

`TAB` and `S-TAB` go between the fields, and `RET` (or `M-TAB`) on the
Project picks it.
The context is written in org, with org's keys: `TAB` there folds a
heading, `M-RET` adds a list item, and the agent gets the text as you
wrote it.
`C-c C-c` creates the task and puts a link to it in the heading's text
(the file is left unsaved); `C-c C-k` puts the draft away. The Project
comes from a `VARDE_PROJECT` property, own or inherited, else the
repository the file is in. On a heading that already links an open
task, it offers to go to that instead.
A heading's `SCHEDULED:` goes into `When:`, so varde starts the task
then; clear it to start the task yourself. It is copied once: `e` on
the board changes the schedule later, not the org timestamp.

**Picking a project** is one picker everywhere, the terminal board's
in the minibuffer: the form's Project, a new task with none, `m` on the
board, and a first start whose project is unsure. It offers the projects
varde knows as `name  ~/dir`, the likeliest first, so RET takes it: the
form's Project, else the task at point's, else the board's. `m` offers
the task's own project last. Typing narrows them, in your completion
UI's way (vertico, ivy and the like keep varde's order); a directory
typed (`~`, `/` or `.` first) completes to the directories in it, and
`another directory…` asks for one with `read-directory-name`.

**Marking a heading DONE** offers to close the open tasks its text
links (`org-varde-close-with-heading`: `ask`, `always` or `never`).
Closing a task never changes the heading.

**A review.** A dynamic block lists tasks with their cairn's Status;
`C-c C-c` on its first line fills it:

```
#+BEGIN: varde :project ~/src/app :state all :closed 7d
|   | Task                                           | Project | Status         |     Closed |
|---+------------------------------------------------+---------+----------------+------------|
| ○ | [[varde:kvdcpq][Write onboarding docs]]        | app     | Outline agreed |            |
| ✓ | [[varde:hxj30t][Remove the old feature flags]] | app     | Flags removed  | 2026-10-13 |
#+END:
```

`:state` takes `open`, `done` or `all`, and `:closed` `24h`, `7d` or
`2w`.

**From an earlier org-varde.** `M-x org-varde-convert-links` turns
headings with a `VARDE_TASK` property into headings with a link (`C-u`:
in every agenda and `org-directory` file).

## The board

`M-x org-varde-board` opens it, kept to the project of the buffer you
are in (`.` shows every project). It looks and works as the terminal
board does. `?` lists every key.

| Key | Does |
|---|---|
| `RET` | Go to the task's session, starting or resuming it. `R` fresh, `F` fork. |
| `s` | Start it in the background and stay on the board. |
| `TAB` / `S-TAB` | The next / previous task that needs you. |
| `v` | View the task and its cairn. |
| `n` / `e` | New task / edit its title, project and context, in a form: `Title:`, `Project:` and `When:` (when varde starts it: `at mon 09:00`, `every weekday 09:00`; empty, never) above the line, the context below. An empty Title has a model name the task; a Project changed in `e` moves it, as `m` does. `C-c C-c` saves, `C-c C-k` puts it away. |
| `d` / `P` / `x` | Done / park / drop. `u` reopens. |
| `a` | Accept an Inbox task. |
| `m` | Move it to another project. |
| `y` | Copy a link to the task. |
| `/` | Recall (`C-u /`: every project). |
| `=` | Its changes, in magit or `vc-dir`. |
| `E` | Edit the whole cairn as text. |
| `N` | A session of your own. |
| `z` | Fold the group. |
| `g` / `q` | Refresh / quit. |

`j`/`k` move between tasks, `{`/`}` between groups. `d`, `P`, `x`, `a`
and `s` act on every task in the region. The keys are in
`org-varde-board-mode-map` and `org-varde-show-mode-map`.

Where Emacs has a key already, it differs from the terminal:

| Terminal | Emacs | evil |
|---|---|---|
| `gg` / `G` | `M-<` / `M->` | `gg` / `G` |
| `C-d` / `C-u` | `C-v` / `M-v` | `C-d` / `C-u` |
| `C-l` refresh | `g` | `gr` |
| `z` fold | `z`, `TAB` on a heading | `za` |
| `Esc` | `q` | `q` |
| space marks | the region | `V` |
| `v` view | `v` | `K` |
| `J` / `K` in a view | `J` / `K` | `C-j` / `C-k` |

The buffers you write in start in evil's insert state; `:w` and `ZZ`
save, `ZQ` cancels.

## The view

`v` on the board, or following a `varde:` link, opens the view. Its
**Task** tab shows where the task stands, its cairn, its context, its
sessions and its git state; its **Project** tab shows the project's
cairn. A `›` marks a line you wrote, which updates keep; a yellow `?`
one about code that is gone.

| Key | Does |
|---|---|
| `n`/`p`, `j`/`k` | The next / previous line. |
| `[` / `]` | The other tab. |
| `e` | Edit the line at point. |
| `o` / `a` | Add a line to this section / to one picked by key. |
| `x` / `u` | Remove the line / bring it back. |
| `E` | The whole cairn as text. |
| `J` / `K` | The next / previous task. |
| `RET` | Go to the session. |
| `d`, `P`, `X`, `s`, `/` | As on the board (`X` drops). |
| `q` | Close. |

A line is typed in the minibuffer. `C-g` keeps it as a draft, and the
same key on the same line takes it up again.

## Moving between sessions

Each task's session runs in a terminal of its own, reused every time
you go to it, in a Doom workspace of its own when there are
workspaces (`org-varde-workspaces` nil keeps one).

- `RET` on the board or in a view goes to the session.
- `org-varde-next-needs-you` goes, from anywhere, to the next task that
  needs you. Bind it.
- `org-varde-board` comes back to the board, in its own workspace.
- In `term`, `C-c` is term's escape; the keys under `C-c` you bound
  org-varde's commands to still work.
- Under evil, a session's terminal wants insert state, as
  evil-collection sets it.

Closing a task closes its terminal and its workspace.

A `claude` session you start outside Emacs, or with `N`, becomes an
Inbox task once you ask it something. It stays in the Inbox until you
accept it (`a`), start it, or close it.

**Scheduled tasks** sit in the Scheduled group, showing when they start
(`↻ Mon 09:00`). varde runs them in the background; once done, one
waits in Needs you and `RET` carries it on in its terminal, as any task.
`RET` while it still works asks whether to take it over.
