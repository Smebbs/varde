# Proposal: loops and trails

Status: built as a beta (design §6.7); this is the proposal it came
from. Recipes use `[[steps]]` with an `id` rather than `[steps.<id>]`,
so their order is kept. It changes parts of `varde-design.md`; §9 lists
each change. §1 and §12 sum up the research behind it. A round of the
loop is one session; a trail is a graph of tasks; a recipe is a trail
written down to reuse.

## 0. The idea in one paragraph

Agents already run loops and graphs *inside* a session: `/goal` and
Stop hooks loop a conversation, and subagents, agent teams and Workflows
fan work out within one. Everything they do is ephemeral. It dies with
the session, resumes only in that session, remembers nothing across
sessions, keeps status the model reports itself, and has no place where
a person sees it. varde already owns what those lack: durable tasks,
derived status, a worktree per task, bounded memory handed to each
fresh session, background runs, and messages between sessions. So the
split is:

> **Inner loops belong to the agent. Outer loops and graphs belong to
> varde.** One round of a varde loop is a whole session, and one node
> of a varde graph is a whole task.

This takes four primitives, each useful alone and each building on the
one before, and recipes (§13) to reuse them:

| Primitive | What it is | Is a fact, or derived |
|---|---|---|
| **Profile** | a named agent + model + effort | config; each run records the one it used |
| **Check** | how varde verifies a task: a command, then optionally a review by another model | stored; its results are events |
| **Loop** | a task that starts fresh sessions until its check passes, within a budget | stored settings; its round, state and stop reason are derived |
| **Trail** | tasks joined by `after` edges; a task is ready when what it waits on is verified | the edges are stored; ready, waiting and behind are derived |

No daemon, no model that decides what starts, and varde still never
merges into the base. `varde tick`, the schedule's heartbeat, becomes
the one engine: a pure function says what starts next.

## 1. What the research says, in five lessons

1. **The verifier is the loop.** Cherny calls verification "probably the
   most important thing" (his "2–3x quality" figure is his own estimate,
   not a measurement). Anthropic says the verifier must be "nearly
   perfect, otherwise Claude will solve the wrong problem" (C compiler,
   Feb 2026). Best-of-N and loops pay only when a check is strong:
   choosing among candidates is where the gain is lost (69.8% of
   CodeMonkeys tasks had a correct candidate, 57.4% were solved after
   selection), and weak tests turn more samples into more false passes.
2. **"The agent says done" is not "verified done".** Agents
   "confidently prais[e] the work", so Anthropic's harness separates the
   evaluator. Reward hacking is real (METR: 30.4% of RE-Bench runs, mainly
   o3), and Anthropic's system cards report attempted hacking 3–6x higher
   on impossible tasks. Detecting "no progress"
   and handing back to a person is a safety measure, not just a cost one.
   Anthropic: "It is unacceptable to remove or edit tests."
3. **Fresh context beats long context.** Ralph's `while :; do … ; done`,
   Anthropic's initializer + progress file, and the context resets
   their Mar 2026 harness used for Opus 4.5 all start each round fresh
   from durable state
   (git, a progress file, a pass/fail list). varde's cairn, folded
   before the next fresh run, is exactly that, and it is bounded,
   anchored and measured.
4. **Graphs: default to one writer.** Multi-agent costs about 15x the
   tokens of chat (a single agent about 4x); Anthropic calls coding a poor fit for heavy parallelism, and
   Cognition's 2026 revision says "writes stay single-threaded". What
   works: independent work in separate trees, a reviewer with a clean
   context working from the diff, a stronger "smart friend" when stuck,
   and a serialized integrate step. Agent teams document the gaps a
   durable store fills: no resume, task status that lags and blocks
   dependents.
5. **Models and effort are roles.** Effort (`low`…`max`) is now the main
   control (for Fable 5.2 the docs say most of the gain is there by
   `high`). The patterns that pay:
   a strong planner with a cheaper executor (Aider architect/editor,
   `opusplan`, Anthropic's advisor tool), escalation when stuck, a
   verifier at least as strong as the generator or from another model
   family (judges favour their own output), and a cheap model only where
   evidence can be checked. Model names change every few months, so
   configs should name roles and aliases, never IDs.

## 2. Where each layer lives

| Layer | Owner | Lifetime | Example |
|---|---|---|---|
| a turn's tools and subagents | the agent | one turn | Claude's subagents, a Workflow script |
| a loop within one conversation | the agent | one session | `/goal`, a Stop hook, pi's `continue` |
| **a loop of sessions** | **varde** | until verified, stuck or out of budget | Ralph, fix-until-green |
| **a graph of tasks** | **varde** | until every node is verified or closed | spec → build ∥ build → integrate |
| the plan of the graph | a session, then you | proposal → accepted | a planner task proposes a trail |

A rule of thumb a session can be told: *if a piece of work needs its own
tree, memory, a person who might attach, or to survive a restart, it is
a task; otherwise it is a subagent.* varde never looks inside a
session's own loops, and a node's session may use them freely.

## 3. Profiles

```toml
# ~/.config/varde/config.toml
[profiles]
quick  = { agent = "claude", model = "haiku",  effort = "medium" }  # triage, summaries
build  = { agent = "claude", model = "sonnet", effort = "medium" }  # well-specified work
deep   = { agent = "claude", model = "opus",   effort = "high" }    # planning, hard debugging
review = { agent = "pi", model = "<provider/model>", effort = "high" }  # a judge from another family

[roles]   # what each role uses unless told otherwise
work = "build"
plan = "deep"
review = "review"
memory = "quick"
```

- **Where it applies:** `varde new|start|attach --profile P`, a task's
  default (`varde set --profile`), a loop's escalation list, and a
  check's reviewer. `memory.model` becomes the `memory` role, which
  takes only a model: memory calls pin their own effort (§7.2).
- **The adapter carries it.** `Launch` gains `model` and `effort`:
  Claude `--model`/`--effort` (both in 2.1.280), pi `--model` and its
  thinking level. Effort is held for the whole session, since changing
  it mid-way can invalidate the prompt cache.
- **Each run records** the profile, the model the agent says it ran
  (from the transcript), tokens, and cost when the agent reports one
  (`total_cost_usd` in `-p`'s JSON). varde keeps no price table.
- **Aliases, not IDs.** The shipped defaults use the agent's aliases, so
  they survive a release. A profile the agent refuses shows on its first
  run's failure; `doctor` flags a review profile that names the same
  model as the work it judges.
- With no profiles configured, varde behaves as today.

## 4. Checks

A check is how varde knows, rather than is told, that work is done.

```
varde check <task> -- cargo test -p parser             # a command
varde check <task> --review [PROFILE] [--rubric FILE]   # a model's review
varde check <task> --guard 'tests/**' --guard Cargo.lock
varde check <task> --again 5                           # 5 passes in a row (flaky work)
varde check <task> --now | --off
varde approve <task>                                   # you say it is good
```

- **Its source is you, never the repository.** The command is stored in
  varde, or comes from a repository's git config (`varde.check`), as
  `varde.setup` does. Nothing committed is ever run (Q17; §12 later
  allows a repository's recipes once you approve their hash).
- **Three kinds, combinable.** A command (its exit code), a review (a
  model with your rubric), or both, in which case the review runs only
  after the command passes, so a judge never stands in for tests. A
  task with no check is verified by `varde approve` alone.
- **Runs detached, never in a hook.** A check is a `job` of kind `check`,
  run by `varde verify <task>` in the task's checkout with a timeout and
  `VARDE_INTERNAL=1`: one queued per task, a newer request replacing it.
  It is queued when a loop's round ends, by `--now`, and (if §14 Q3 says
  yes) when an interactive turn ends, where the hook only queues it.
  Checks count toward `loops.parallel` (§7), since three `cargo test`s
  share one machine.
- **What it checked.** In git, the tree: `git add -A` into a temporary
  index (`GIT_INDEX_FILE`), then `write-tree`, so uncommitted and
  untracked files count. It is taken before and after the command; if
  they differ, someone edited meanwhile and the result is `unchecked`.
  Outside git there is no tree, so a check stands for the time it ran.
- **The result is an event** on the run it followed: `pass`, `fail` or
  `error`, the exit code, its duration, the tree, a **fingerprint** of
  the failure (normalised output, hashed), and the output in a file
  beside the store.
- **Verified is derived, never stored.** The newest check passed and, in
  git, the tree there now is the tree it checked; outside git, no run of
  the task has started since. The tree "there now" is computed only by
  `verify` and by the detached `git-refresh`, and cached in `git_state`,
  so the board compares two cached hashes and spawns nothing (rule 3,
  Q35). Rows show `✓`, `✗ 3 failing`, `… checking`.
- **A review is an internal call, not a session.** Like a memory update
  (§7.2) it is a headless call of the review profile with read-only
  tools, never a run: it is not on the board, attach never resumes it,
  and no cairn fold reads it. It gets the task's context (the spec), the
  diff from its base, the command's output and your rubric file. It
  answers through structured output (`--json-schema`: `{verdict,
  findings[]}`; pi: a strict last line), and a finding counts only if it
  cites a file and line. Its verdict is the check's event; its text is
  the output file, which the Loop tab shows.
- **The guard.** A run whose diff touches a guarded path is not failed,
  it is *flagged*: the row says `touched tests/…`, any loop stops, and
  you judge. Removing or weakening a test is the classic hack, and
  whether it was one is yours to call. Default guards (§12): tests,
  dependency manifests, toolchain pins, CI config, and skip markers
  added to tests.
- **`varde approve`** records that you looked at this task's work: who,
  when, and the tree. It is the `you` check, and what an `--approved`
  edge waits on (§6). A change to the tree after it makes it lapse,
  derived as verified is.

## 5. Loops

```
varde loop <task> [--check CMD] [--max 3] [--budget 40turns|90m|5usd]
                  [--escalate deep,deep:xhigh]
varde loop <task> --off
```

A loop needs a check (`--check` is shorthand for `varde check <task> --
CMD`). It means: **until the check passes, each failed check starts the
next round** as a fresh background run (§6.5's machinery: print mode,
hooks track it). A round is given:

- the cairn (the tick already waits for the last run's fold, so each
  round has the previous one's lessons: the progress file, bounded),
- `Round 2 of 3. The check \`cargo test -p parser\` failed:` and the
  failure's tail,
- the check command, so it can run it itself. varde re-verifies anyway.

With only a review check, nothing deterministic decides, so a loop is
capped at 2 rounds and always ends with you, whatever `--max` says.

Its state is **derived** from the runs it started and their check
events, never stored. Where several apply, the first in this table
wins:

| State | When | Where on the board |
|---|---|---|
| **flagged** | a round touched the guard | Needs you, with the paths |
| **error** | the check could not run, or two rounds in a row died on an agent error (rate limit, API) | Needs you, with the error |
| **verified** | the check passed for the tree there now | Needs you, unread: look and land |
| **spent** | `--max`, `--budget` or time ran out | Needs you |
| **stuck** | two rounds failed with the same fingerprint, the tree unchanged between them, and no profile left to escalate to | Needs you, with the failure |
| **asked** | a round ended on a question to you: `AskUserQuestion`, a permission it could not get, or a last message that asks | Needs you, with what it asked |
| iterating `↺ 2/3` | a round or its check is at work, or the next is due | Working |
| paused | you took it over, or it is parked | as any task |

- **Progress is the tree, not commits.** Agents in print mode often do
  not commit, and work outside git never does, so "did anything change"
  is the tree hash (or, outside git, the files' mtimes in the task's
  directory).
- **What counts.** Only rounds the loop started count toward `--max`
  and the budget; a session of yours never does. A round that died on
  an agent error counts once. Turns and minutes are the default budget,
  since dollars mean little on a subscription. A loop's rounds run with
  `--output-format json` so turns and `total_cost_usd` are recorded.
- **Escalation before giving up.** When a round would be `stuck` and the
  `--escalate` list has a profile left, the next round runs on it
  (`deep:xhigh` is the `deep` profile at `xhigh` effort). A cheap model
  does the routine rounds and a strong one gets the hard spot.
- **Taking over pauses it.** `attach` stops the round under way (as
  today) and pauses the loop. `varde loop <task>` resumes it. A loop
  never starts a round while a session of yours is open.
- **Hand-overs keep it moving.** On a task handed to the steward (§6.6),
  permission asks go to the steward and your `--allow` rules, so a round
  does not wait for you. What is never delegated stays so: a round that
  needs one of those steps ends `asked`.
- **The simplest loop** is Ralph's: a task, a check, `varde loop`. The
  difference from `while :; do claude; done` is that it stops when the
  check passes, not when the model says so; it notices when it is stuck;
  it remembers through a capped, checked cairn instead of a growing
  file; and it lands in Needs you.

## 6. Trails

A trail is tasks joined by edges. It needs no entity of its own: it is
the connected set.

```
varde new "Store layer" --after spec          # or: varde after <task> <dep>…
varde new "Integrate"   --after store,api --approved
varde after <task> --off <dep>
varde trail <task> [--start]                  # draw it; --start sets it going
```

- **Edges wait on *verified*, not done.** Verified is a machine fact;
  done is yours. `--after A` waits until A is verified. `--after A
  --approved` waits until you `varde approve A`: a human gate is an edge
  kind. Never on `done`: a day after a task is done, an unlanded
  worktree is put away (Q40), which would take its branch from its
  dependents. A cycle is refused.
- **Ready is derived**, as status is: every edge satisfied. In a started
  trail the tick starts a ready task in the background, as a schedule
  fires. Otherwise it stays in Open labelled `ready`, for you to start.
  Waiting tasks are in Open too, labelled `⧗ <short>` (what they wait
  on).
- **Worktrees build on their upstream.** One upstream: the worktree is
  cut from its branch (a stack). Several: cut from the base, and the
  session is told which branches to merge, since **varde still merges
  nothing**. An upstream's worktree and branch are kept while any task
  that waits on it is open, done or not. A task whose upstream branch
  has moved on shows `behind <short>` (from the git refresh), and its
  next session is told.
- **What flows along an edge** is what memory already carries, through
  the same delivery path and word budget: per upstream, its title, its
  cairn's Status and Findings, its branch, and its last report (read
  from its transcript, as `said` does). It generalises the origin block
  (§7.3), and takes room after the origin and the task's own cairn,
  before project memory and related work. No full transcripts: context
  isolation is the point.
- **Failure stays where it happened.** An upstream that is stuck or
  flagged is in Needs you. Its dependents keep waiting and say on what.
- **Who draws the trail.** You, with the commands above, or a planner: a
  task on the `plan` role whose session decomposes the work and files
  each node with `varde new --from <run> --after …`. The nodes wait in
  the Inbox as one row with the trail drawn beneath it, and **accepting
  the root accepts the trail**, so a whole plan costs you one look and
  nothing starts on a model's word (§6.4 holds). The planner's playbook
  comes from the binary (`varde guide trail`), as the steward's does:
  few nodes (3–6), split only work that touches separate files, end
  with an integrate node, give each node a check.
- **Recipes (§13).** A trail written down once and reused:
  `varde new "Offline sync" --recipe feature`.

### Example

```
varde new "Spec: offline sync" --profile deep
varde check spec --review                     # reviewed by the other family
varde new "Store layer" --after spec --approved --profile build
varde new "Sync API"    --after spec --approved --profile build
varde check store --guard 'crates/store/tests/**' -- cargo test -p store
varde check api -- cargo test -p api
varde loop store --escalate deep ; varde loop api --escalate deep
varde new "Integrate" --after store,api --approved
varde trail spec --start
```

The spec runs and is reviewed. When you approve it, store and api start
in parallel, each in its own worktree cut from the spec's branch, each
looping until green. Integrate waits until you approve both, then
merges their branches in its own tree. You land it.

## 7. The engine: `tick`, no daemon

- **One pure function**, table-tested as `plan()` is:
  `next(tasks, edges, loops, schedules, runs, checks, now) -> [Start {task,
  profile, prompt}] + [Hold {task, why}]`. Schedules, loop rounds and
  ready trail nodes are three inputs to one decision. Every "why is this
  not running" answer comes from the same function (`show`, `doctor`).
- **Woken by what changes:** the heartbeat each minute, a run's exit
  (`varde run` starts a detached tick as it ends), and a job's end (a
  fold or a check). A tick that finds another holding the lock leaves a
  `tick-again` mark, and the holder runs once more before it lets go, so
  no wake is lost. The next round starts once its fold and check are
  in, in seconds, without a daemon.
- **Loops need the heartbeat** (`varde schedule --install`) as the
  backstop; `varde loop` and `doctor` say so when it is missing.
- **Bounded.** `loops.parallel` (default 3) caps background runs and
  checks together, schedules included; the rest queue. The decision and
  the runs it starts are written in one transaction (today `start`'s
  preparing and the background mark are two statements; this joins
  them).
- **No model decides what starts.** Models are nodes, checks and
  reviewers; scheduling is code.

## 8. Measuring it

`varde eval` gains a loops-and-trails section, offline and read-only, as
memory's (§7.7):

- **Loops:** rounds to verified; cost and minutes per verified task;
  end states by reason; escalations, and whether the escalated round
  passed.
- **Checks:** *verified, then reworked*: a task you reopened, or whose
  branch you changed, after it passed. That is the false-pass rate,
  the number that says whether a check can be trusted. Also how often
  review and command disagree, guard flags, and `pass^k` from
  `varde check --again k`.
- **Profiles:** for each profile and role, rounds, cost and rework, so
  "is sonnet/medium enough for this repo" has an answer.
- **Recipes:** the same, per recipe version (§13).
- **People:** interventions per verified task (takeovers, answers,
  asks), the number loops exist to bring down.

## 9. What changes in the design

| Where | Today | Proposed |
|---|---|---|
| §2 non-goals | "orchestration" | "an orchestrator agent or process": varde runs loops and trails as data through `tick`, and no model decides what starts |
| §6.4 | deferred work only on your word | unchanged; a planner's trail waits in the Inbox and is accepted as one |
| §6.5 | schedules start background runs; `tick` from the heartbeat | loop rounds and ready trail nodes use the same machinery; `tick` becomes `next()`, is also woken by a run's exit and a job's end, never loses a wake, and caps background work |
| §6.6 | never-delegated steps ask you | unchanged; in an unattended round one ends the round `asked` |
| §7.3 | origin block | upstream blocks, after the cairn and before project memory |
| §8 Goals | "varde has no loop of its own" | inner loops stay the agent's; varde runs the loop of sessions |
| Q10 | varde opens no PR | unchanged for varde; a recipe's `draft-pr` output asks the session to, on your permissions (§13.3) |
| Q17 | nothing committed is run | later, a repository's recipes run once you approve their hash (new table `recipe_trust`) |
| Q29 | row labels | new labels for checks, loops and trails; no new groups |
| Q40 | an unlanded worktree is put away a day after done | not while a task that waits on it is open |
| config | `memory.model` | profiles and roles; the `memory` role takes only a model, since memory calls pin their own effort |
| decisions | zvqhsm #260, 4zx2jt: no loop runner | reversed for the *outer* loop (new Q42–Q46); still no inner loop, no `start --goal` |

New decisions: **Q42 Checks** (yours, never the repository's; verified
derived per tree; command before review; the review an internal call;
guard flags, never fails; `approve`), **Q43 Loops** (rounds are fresh
sessions; state derived with a fixed precedence; progress is the tree;
escalate, then stop), **Q44 Trails** (edges wait on verified or
approved, never done; varde merges nothing; planner proposals accepted
as one), **Q45 Profiles** (roles and aliases, effort held per session,
reviewer of another model), **Q46 Recipes** (data, not code; versioned
by hash; yours run, a repository's once approved).

## 10. Deliberately left out

- **Landing on its own:** no auto-merge, and no write to the base (Q10).
  varde opens no PR itself. Verified lands in Needs you.
- **varde merging branches** for a fan-in. The integrate session does it.
- **An LLM in scheduling,** or a steward that runs the graph. The
  steward answers asks and helps you draw trails; `tick` runs them.
- **Inner loops** (`/goal`, Stop-hook continuations) and in-session
  graphs (Workflows, agent teams): the agent's, already good, used
  freely inside a node.
- **Scripting inside varde** (embedded JS or Lua). Logic varde has no
  word for goes in your check command or prompt file (§13.1).
- **Best-of-N** at first. It pays only with a strong selector; once
  `eval` shows checks are trustworthy, `varde loop --wide 3` (three forks
  in three worktrees, the check filters, a review picks) is a small
  step. Not before.
- **Running committed checks or recipes without your approval** of
  their hash, a price table, a live graph editor, and a new board group
  (Q29: rows say loop and trail state in their labels).

## 11. Phases, each shippable alone

1. **Profiles** — config, `Launch` gains model and effort, runs record
   model, tokens and cost. Useful on day one: `varde start --profile deep`.
2. **Checks** — command and review, verified per tree, the guard,
   `approve`, board labels. Useful without loops: "is it green?" on
   every row.
3. **Loops** — `next()` in `tick`, its wakes, the round prompt, the
   states, escalation, eval's loop section.
4. **Trails** — edges, ready, stacked worktrees, upstream delivery, the
   planner playbook, accept-as-one.
5. **Recipes** — built-ins and yours, `recipe show`, eval per version.
6. **Later, each on its own evidence** — `red_first`, the andon, fan-out
   over a list, best-of-N, PR comments as a trigger and as feedback,
   `say:` output, a repository's recipes by hash.

## 12. From the factory research

A second round of research mapped the proposal onto "software factories"
and reusable loops. Every factory that works has the same skeleton:
intake, decompose, isolated worker, verifier gate, merge step, land,
plus a supervisor. varde is the ledger and the line, not the foreman.
What that round added is folded into §4–§11; what remains here is why,
and what waits:

- **The oracle decides.** Success tracks how strong the check is, not
  the topology, which is why a review-only loop gets 2 rounds and ends
  with you (§5).
- **Default caps.** `--max` defaults to 3, near Stripe's 2 CI rounds; no
  source shows an open-ended "loop until green" in production.
- **Default guards** beyond tests (§4): Spotify's agents commented tests
  out and downgraded Java to get green.
- **Waste.** A loop's round that claims done with no change to the tree
  is flagged. Only a loop's rounds: a scheduled run that changed nothing
  and says so is the quiet run of §6.5, not waste.
- **Later, each when the evidence asks for it** (§11 phase 6):
  - **`red_first`** for bug fixes: the new test must fail at the
    worktree's fork point, run in a throwaway worktree, never your
    checkout.
  - **The andon:** a check on the base, run by the refresh, whose
    failure holds new starts in that project's trails until it passes
    or you clear it (one stored fact).
  - **Fan-out over a list:** one recipe across N items, capped, then a
    serialised integrate.
  - **PR comments** as a trigger and as feedback. Third-party text
    starting an unattended run is an injection path, so it waits.
- **Lights dim, not out.** No level 5. StrongDM's "code must not be
  reviewed by humans" is the exception; every company that publishes
  production numbers still has a person merge, and on dotnet PRs with a
  person involved merged 86% of the time against 55% without. Landing
  stays yours.

## 13. Many loops: customizing, seeing, and work that isn't code

### 13.1 One loop, seven parts

Variety comes from combining a few parts, not from a scripting
language. Every loop, and every step of a recipe, is these seven
(*later* marks what §11 ships last):

| Part | Choices |
|---|---|
| **Trigger:** when it starts | by hand, a schedule (§6.5's rules), `after` an upstream is verified or approved; *later* the base going red, a PR comment |
| **Prompt:** what it is asked | a markdown file with `{{vars}}`, skill-compatible |
| **Profile:** who works, and how hard | any profile; an escalation list (`build,deep,deep:xhigh`) |
| **Check:** how varde knows | `cmd`, `again`, `review` (a model and your `rubric` file), `you` (`varde approve`), none; *later* `red_first` |
| **Feedback:** what the next round gets | the check's output, review findings, your note (`varde note`/`edit`); *later* PR comments |
| **Stop:** when it gives up | `max`, a budget in turns, minutes or dollars, the stuck rule |
| **Output:** where the result goes | `branch` (the default: nothing leaves your machine), `draft-pr`, `file:<path>` (a draft in the task's directory), `report` (the cairn and last report only); *later* `say:<task>` |

A guard (paths that must not change) rides along with any check.

**The escape hatch is a command, not code inside varde.** Any logic
varde has no word for goes in a check command or a prompt file, both
yours. `next()` stays a pure function, small enough to table-test, and a
recipe stays data you can read. Rejected: embedded JS or Lua, as
Workflows scripts are. They are expressive, but they are a program varde
would have to run, resume and trust.

### 13.2 Four levels of customizing

1. **Flags on one task:** `varde loop t --check 'make test' --max 3`,
   or `--set <step>.<key>=<value>` on a recipe for one task.
2. **Built-ins from the binary** (Q12, as the steward's guides are):
   `fix-until-green`, `bugfix`, `review`, `draft-revise`, `research`.
3. **Your recipes** in `~/.config/varde/recipes/`. `varde recipe edit
   bugfix` copies a built-in there to change; yours shadows it.
   `varde recipe list`, `varde new "…" --recipe bugfix --var issue=gh:123`.
4. **Distilled:** `varde recipe save <task>` turns a trail that went well
   into a recipe.

**A recipe, every key.** Anything left out takes the default shown.

```toml
label = "bugfix"                 # a name for people; the version is the content hash
[vars.issue]                     # {{issue}} in prompts; --var issue=…
required = true

[steps.fix]
needs = ["repro"]                # upstream steps; default none
use = "fix-until-green"          # start from that recipe's step; keys here override it
profile = "build"                # default: the work role
prompt = "prompts/fix.md"        # default: the task's context
check = { cmd = "cargo test", review = false, again = 1 }  # cmd default: git config varde.check
loop = { max = 3, escalate = ["deep"], budget = "40turns" } # omit for one run, no loop
guard = ["tests/**"]             # default: the default guards (§4)
gate = "verified"                # what dependents wait on: verified | approved
output = "branch"                # branch | draft-pr | file:<path> | report
```

- **Versions are hashes.** A recipe's version is its content hash (with
  `label` for people); each task records the recipe and hash it was made
  from, and `varde eval --recipe` reports per version: rounds to
  verified, cost, stuck and asked rates, and *verified, then reworked*.
  Customizing is a loop too, and that report is its check.
- **`use` inlines.** A step that uses another recipe's step starts from
  it, and its own keys win. `--set` wins over both, for that task only.
- **A task keeps what it was made with.** The resolved recipe is stored
  with the task and shown in its view, so editing a recipe never
  changes a task already under way.

### 13.3 One change, end to end

Say the built-in `bugfix` should end in a draft PR, not a branch you
land yourself:

```
$ varde recipe edit bugfix          # copies the built-in to your config, opens $EDITOR
```
```diff
 [steps.review]
 profile = "review"
 check = { review = true, rubric = "rubrics/bugfix.md" }
-output = "branch"
+output = "draft-pr"
```

Or for one task only: `varde new "Fix login race" --recipe bugfix --set
review.output=draft-pr`. Either way, `varde recipe show` shows the result
before anything runs, and says where each value came from (as `git
config --show-origin` does):

```
$ varde recipe show bugfix --origin
bugfix · yours (a3f9c1), from built-in (77b2e0)
repro        build        check cmd                                built-in
└─ fix       build→deep   ↺ max 3 · check cargo test (git config)  built-in
   └─ review review       check review · output draft-pr → you    yours :6
```

- **varde opens nothing itself.** `draft-pr` tells the last step's
  session to push its branch and open a draft PR. Whether it may is
  yours: pushing is reserved for you (§6.6) and checked before any
  `--allow` rule, so neither the steward nor a varde rule can grant it.
  If your own agent settings allow `git push` for that launch, it opens
  the draft. Otherwise the round ends `asked`, with the exact `git push`
  and `gh pr create --draft` commands ready for you. A recipe never
  widens permissions.
- **`file:` is the draft for documents.** `output = "file:drafts/{{slug}}.md"`
  leaves a draft in the task's directory and its path in the report.

### 13.4 Seeing it

varde renders; clients draw (as with the board). Every view has a
`--json` that clients render without deciding anything.

- **The board row** (Q29: no new groups) says a loop's or trail's state
  in its label: `↺ 2/3 ✗ 1 failing`, `⧗ spec`, `✓ verified in 3`,
  `⚑ touched Cargo.lock`.
- **The task view gains a Loop tab:** one line per round, with its
  profile, minutes, cost, verdict, whether the tree and the failure
  changed, and the review's text. Escalation shows where the profile
  changes.
- **`varde trail <task>`** draws the graph as a tree, as `git log
  --graph` does, each node with its state. The TUI opens it with `t`,
  Emacs in a buffer, and an org dynamic block (`#+BEGIN: varde-trail`)
  can show it in a note. A planner's trail in the Inbox is drawn the
  same way under its root's row.
- **`varde recipe show <name>`** draws a recipe with the same renderer
  before anything runs. A recipe is a trail that has not run yet. The
  header carries its eval line, and `--origin` says where each value
  came from.
- **`--format mermaid|dot`** on `trail` and `recipe show`, for docs and
  pages.

### 13.5 Work that isn't code

Nothing in loops or trails needs code. varde already runs tasks outside
git, with no worktree. What changes is what the parts are:

- **Workspace.** A task's directory: notes, drafts, an org or obsidian
  folder. A git repository of documents still gets a worktree, so a
  draft cannot clobber the original.
- **Verified,** outside git, means the newest check passed and no run
  has started since (§4); progress is whether the directory's files
  changed (§5).
- **Checks.** Still commands where possible: `vale` or a spell check, a
  word or section budget, a link checker, a `pandoc` or LaTeX build, a
  script that checks the numbers reconcile, a ledger that balances, a
  schema that validates. Where only judgement will do, a `review` check
  with your rubric file, capped at 2 rounds and ending with you.
- **Output.** A file in the task's directory, or the cairn and last
  report. varde never writes into your org files (Q31). It links to
  them.
- **Typical shapes:** draft → critique → revise (`draft-revise`), a
  research brief whose citations a link checker verifies, a weekly
  personal review on a schedule that is quiet when there is nothing to
  do, a report whose figures a script reconciles.

## 14. Open questions for you

1. **Visibility.** Rounds run in the background (print mode, take over
   to watch), or in tmux windows you can glance at?
2. **Edge default.** Wait on *verified* (fast, machine) unless told
   otherwise, with spec and plan steps defaulting to *approved*?
3. **Checks on interactive sessions.** Queue the check when a turn ends
   in a changed tree (a live `✓`), or only for loops and on demand?
4. **Budgets.** Turns and minutes by default, dollars only when asked?
5. **Names.** "Trail" (a line of cairns), "loop", "round" and "recipe":
   keep them?
