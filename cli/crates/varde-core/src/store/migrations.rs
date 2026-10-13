//! The store's schema, as the numbered migrations that build it.

/// Numbered migrations; `user_version` records how many have run.
/// Append only — never edit one that has shipped. The first is the
/// schema varde 1.0.0 shipped with.
pub(super) const MIGRATIONS: &[&str] = &[
    r#"
CREATE TABLE task (
  id       TEXT PRIMARY KEY,
  title    TEXT NOT NULL,
  body     TEXT,
  project  TEXT,
  state    TEXT NOT NULL DEFAULT 'open' CHECK (state IN ('open','done','dropped','parked')),
  inbox    INTEGER NOT NULL DEFAULT 0,
  external TEXT,
  created  INTEGER NOT NULL,
  closed   INTEGER,
  origin   TEXT REFERENCES task(id),
  changed  INTEGER
);
-- When a task's state last changed, however it was changed, so a client
-- that keeps its own copy of the state can tell which side changed last.
-- A trigger, so no writer can forget it.
CREATE TRIGGER task_changed AFTER UPDATE OF state, inbox, external ON task
WHEN OLD.state IS NOT NEW.state OR OLD.inbox IS NOT NEW.inbox OR OLD.external IS NOT NEW.external
BEGIN
  UPDATE task SET changed = CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER) WHERE id = NEW.id;
END;

CREATE TABLE run (
  id           TEXT PRIMARY KEY,
  task         TEXT REFERENCES task(id),
  agent        TEXT NOT NULL,
  native_id    TEXT NOT NULL,
  parent_run   TEXT REFERENCES run(id),
  relation     TEXT NOT NULL CHECK (relation IN ('fresh','resume','fork')),
  cwd          TEXT,
  transcript   TEXT,
  started      INTEGER NOT NULL,
  launch       TEXT NOT NULL DEFAULT 'adopted' CHECK (launch IN ('adopted','managed')),
  prompt       TEXT,
  pid          INTEGER,
  pid_start    INTEGER,
  exit_code    INTEGER,
  cairn_cursor INTEGER NOT NULL DEFAULT 0,
  cairn_at     INTEGER,
  tty          TEXT,
  holdout      INTEGER,
  -- When `tty` was made: a closed terminal's path is soon another's.
  tty_ctime    INTEGER
);
CREATE INDEX run_task ON run(task, started);
CREATE INDEX run_native ON run(agent, native_id, started);

-- `agents` and `shells`: what a turn left running in the background,
-- set on turn ends only.
CREATE TABLE event (
  seq    INTEGER PRIMARY KEY,
  run    TEXT NOT NULL REFERENCES run(id) ON DELETE CASCADE,
  kind   TEXT NOT NULL,
  ts     INTEGER NOT NULL,
  raw    TEXT NOT NULL,
  agents INTEGER,
  shells INTEGER
);
CREATE INDEX event_run_ts ON event(run, ts);

CREATE TABLE worktree (
  path    TEXT PRIMARY KEY,
  task    TEXT NOT NULL REFERENCES task(id),
  repo    TEXT NOT NULL,
  branch  TEXT NOT NULL,
  base    TEXT NOT NULL,
  state   TEXT NOT NULL CHECK (state IN ('active','removed','merged')),
  created INTEGER NOT NULL,
  closed  INTEGER
);
CREATE INDEX worktree_task ON worktree(task, created);

-- A cache of where each task's git work stands, kept by the detached
-- `varde git-refresh`: facts from outside varde, not a stored status.
CREATE TABLE git_state (
  task    TEXT PRIMARY KEY REFERENCES task(id),
  stage   TEXT,
  label   TEXT,
  checks  TEXT,
  facts   TEXT,
  checked INTEGER NOT NULL
);

CREATE TABLE cairn (
  task        TEXT PRIMARY KEY REFERENCES task(id),
  version     INTEGER NOT NULL,
  updated     INTEGER NOT NULL,
  status      TEXT NOT NULL DEFAULT '',
  status_mine INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE cairn_history (
  id     INTEGER PRIMARY KEY,
  task   TEXT NOT NULL REFERENCES task(id),
  doc    TEXT NOT NULL,
  author TEXT NOT NULL CHECK (author IN ('job','user')),
  reason TEXT,
  ts     INTEGER NOT NULL
);
CREATE INDEX cairn_history_task ON cairn_history(task, ts);

CREATE TABLE project_cairn (
  project TEXT PRIMARY KEY,
  status  TEXT NOT NULL DEFAULT '',
  version INTEGER NOT NULL,
  updated INTEGER NOT NULL
);
CREATE TABLE project_cairn_history (
  id      INTEGER PRIMARY KEY,
  project TEXT NOT NULL,
  doc     TEXT NOT NULL,
  author  TEXT NOT NULL CHECK (author IN ('job','user')),
  reason  TEXT,
  ts      INTEGER NOT NULL
);
CREATE INDEX project_cairn_history_project ON project_cairn_history(project, ts);

-- The bullets of task and project cairns, with stable ids. A bullet that
-- leaves stays as history, searchable, with why it left.
CREATE TABLE cairn_item (
  id        INTEGER PRIMARY KEY,
  task      TEXT REFERENCES task(id),
  project   TEXT,
  section   TEXT NOT NULL
            CHECK (section IN ('findings','decisions','open','conventions','gotchas','howto')),
  pos       INTEGER NOT NULL,
  text      TEXT NOT NULL,
  anchor    TEXT,
  mine      INTEGER NOT NULL DEFAULT 0,
  author    TEXT NOT NULL CHECK (author IN ('job','user')),
  run       TEXT REFERENCES run(id),
  created   INTEGER NOT NULL,
  confirmed INTEGER NOT NULL,
  state     TEXT NOT NULL CHECK (state IN ('current','history')),
  reason    TEXT,
  left_at   INTEGER,
  of        INTEGER REFERENCES cairn_item(id),
  stale     INTEGER,
  checked   INTEGER,
  CHECK ((task IS NULL) != (project IS NULL))
);
CREATE INDEX cairn_item_task ON cairn_item(task, state);
CREATE INDEX cairn_item_project ON cairn_item(project, state) WHERE project IS NOT NULL;
CREATE INDEX cairn_item_checked ON cairn_item(coalesce(checked, 0)) WHERE state = 'current' AND anchor IS NOT NULL;

CREATE TABLE note (
  id     TEXT PRIMARY KEY,
  run    TEXT NOT NULL REFERENCES run(id),
  text   TEXT NOT NULL,
  ts     INTEGER NOT NULL,
  folded INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX note_run ON note(run, folded);

-- `unchanged`: the model found nothing worth keeping. `skipped`: no model
-- was asked (superseded, nothing to read, or updates off).
CREATE TABLE job (
  id            TEXT PRIMARY KEY,
  run           TEXT NOT NULL REFERENCES run(id),
  upto          INTEGER NOT NULL,
  kind          TEXT NOT NULL DEFAULT 'fold' CHECK (kind IN ('fold','close','promote')),
  state         TEXT NOT NULL CHECK (state IN ('queued','running','done','unchanged','skipped','failed')),
  error         TEXT,
  created       INTEGER NOT NULL,
  finished      INTEGER,
  attempts      INTEGER NOT NULL DEFAULT 0,
  beat          INTEGER,
  calls         INTEGER,
  input_tokens  INTEGER,
  output_tokens INTEGER,
  repeated      INTEGER,
  like_shown    INTEGER,
  UNIQUE (run, upto, kind)
);
-- What each run was shown of memory, held-out runs included.
CREATE TABLE shown (
  id      INTEGER PRIMARY KEY,
  run     TEXT NOT NULL,
  ts      INTEGER NOT NULL,
  via     TEXT NOT NULL,
  kind    TEXT NOT NULL CHECK (kind IN ('cairn','origin','related','project','holdout')),
  task    TEXT,
  version INTEGER,
  items   TEXT NOT NULL DEFAULT '[]'
);
CREATE INDEX shown_run ON shown(run);

CREATE VIRTUAL TABLE memory_fts USING fts5(
  title, findings, decisions, open, history, notes, memory,
  task UNINDEXED, kind UNINDEXED, ref UNINDEXED, scope UNINDEXED,
  tokenize = 'porter unicode61'
);
-- Reindexing a task or a project finds its rows by these, not by
-- scanning every row: in `memory_fts` itself, task (c7) and scope (c10)
-- are UNINDEXED.
CREATE INDEX memory_fts_task ON memory_fts_content(c7);
CREATE INDEX memory_fts_scope ON memory_fts_content(c10) WHERE c7 IS NULL;
CREATE TABLE recall_log (
  ts         INTEGER NOT NULL,
  query      TEXT NOT NULL,
  hits       INTEGER NOT NULL,
  run        TEXT,
  source     TEXT,
  project    TEXT,
  everywhere INTEGER,
  results    TEXT
);
"#,
    // 2: a title varde cut from an ad-hoc session's first prompt is kept
    // here, so the task may be named once (§6.2) and the first prompt's
    // words stay to be told apart. A title set any other way clears it.
    r#"
ALTER TABLE task ADD COLUMN prompt_title TEXT;
"#,
    // 3: when varde asked a model to name the task (§6.2), so it asks
    // once. A task already closed has no use for a better title: it is
    // taken as asked.
    r#"
ALTER TABLE task ADD COLUMN named INTEGER;
UPDATE task SET named = 0 WHERE prompt_title IS NOT NULL AND state != 'open';
"#,
];
