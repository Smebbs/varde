---
name: varde-loops
description: Use when the user wants work to run on its own until it is verified, in fresh sessions or in parallel, instead of prompting it turn by turn - "loop this until the tests pass", "make this a recipe", "split this big feature", "plan it and fan it out", "check it with /verify-app", "do this read-only and give me a checklist" - or asks how varde's checks, loops, trails, recipes or sandbox work. Sets them up with the varde CLI.
---

# Loops, checks and trails with varde

varde runs work in fresh background sessions, each in its own worktree
and sandbox, until a check says it is done. You set it up; varde runs it;
the user lands it. Before writing a recipe or using a flag you are unsure
of, run `varde recipe guide` (the manual, which matches the installed
varde) and `varde <command> --help`.

## Pick the smallest thing that does the job

| The user wants | Do |
|---|---|
| this task verified, not just done | `varde check <task> -- <cmd>`; add `--review` (another model reads the diff) or `--verify "use /verify-app"` (an agent checks it with its tools and skills) |
| it worked on until green | `varde loop <task> --check '<cmd>' --max 3 --escalate deep` |
| a known shape (bug, feature, writing, research) | `varde recipe list`, then `varde new "<title>" --recipe <name> --var check='<cmd>' --start` |
| a large piece of work, in parallel | `varde new "<title>" --recipe epic --var check='<cmd>' --start`: a plan they approve becomes pieces built at once, then merged |
| a change to a live system, safely | `varde new "<title>" --recipe change-plan --var sandbox=<theirs> --start`: it looks read-only and writes CHECKLIST.md for them to apply |
| a workflow they repeat | write a recipe: `varde recipe guide`, then `varde recipe check FILE` |

## Rules

- A check is a command that exits 0 when the work is right. Prefer the
  narrowest one that proves the task (one test file over the whole suite).
  Without one, ask the user what "done" means; do not invent it.
- Skills are the how: name one in a step's prompt (`/spec`) or in a
  check's `--verify` ("use /verify-app") and every round runs it.
- Guard what the check reads: `--guard 'tests/*'` when a round must not
  change the tests.
- Put the user where only they can judge: a spec or plan step gated
  `approved`. Do not approve, set going or change a check for them unless
  they asked you to.
- Background sessions run in a sandbox. If one needs a login the sandbox
  hides (`gh`, a cloud CLI), say so and let the user pick the sandbox
  (`varde sandbox` shows them); never turn it off on your own.
- Tell the user how to watch it: `varde follow <task>`, or `f` on the
  board.
