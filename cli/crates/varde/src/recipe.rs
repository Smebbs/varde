//! Recipes: a loop or a trail written down once and reused
//! (`docs/proposal-loops-and-graphs.md` §13). A recipe is data, never
//! code: TOML steps, each one task with its profile, prompt, check, loop,
//! guard and output, joined by `needs`. Any logic varde has no word for
//! goes in a check's command or a step's prompt, both yours.
//!
//! Yours live in `~/.config/varde/recipes/<name>.toml` and shadow the
//! built-ins of the same name. A recipe's version is its content's hash,
//! kept on every task made from it, so `varde show` says which version
//! made a task.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use rusqlite::params;
use serde::Deserialize;
use varde_core::project;
use varde_core::task;

use crate::{config, flow, now_ms, Paths};

const BUSY: Duration = Duration::from_secs(2);

/// The recipes varde ships.
const BUILT_IN: [(&str, &str); 7] = [
    ("fix-until-green", include_str!("../recipes/fix-until-green.toml")),
    ("bugfix", include_str!("../recipes/bugfix.toml")),
    ("feature", include_str!("../recipes/feature.toml")),
    ("epic", include_str!("../recipes/epic.toml")),
    ("change-plan", include_str!("../recipes/change-plan.toml")),
    ("draft-revise", include_str!("../recipes/draft-revise.toml")),
    ("research", include_str!("../recipes/research.toml")),
];

/// Where your recipes live.
pub fn dir() -> PathBuf {
    config::home().join("varde/recipes")
}

/// Where a recipe came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    BuiltIn,
    Yours,
    File,
}

impl Source {
    fn name(self) -> &'static str {
        match self {
            Source::BuiltIn => "built-in",
            Source::Yours => "yours",
            Source::File => "file",
        }
    }
}

/// A recipe's text, found by name or path.
struct Found {
    name: String,
    text: String,
    source: Source,
    path: Option<PathBuf>,
}

fn find(name: &str) -> Result<Found> {
    let as_path = Path::new(name);
    if name.ends_with(".toml") || name.contains('/') {
        let text = if let Some(root) = crate::sandbox::bounded_root() {
            crate::sandbox::read_beneath(&root, as_path).map_err(|_| {
                anyhow::anyhow!(
                    "{name} is not a file in this checkout: a session in a sandbox reads no other"
                )
            })?
        } else {
            std::fs::read_to_string(as_path).with_context(|| format!("reading {name}"))?
        };
        let stem = as_path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        return Ok(Found { name: stem, text, source: Source::File, path: Some(as_path.to_path_buf()) });
    }
    let mine = dir().join(format!("{name}.toml"));
    if mine.is_file() {
        return Ok(Found {
            name: name.into(),
            text: std::fs::read_to_string(&mine).with_context(|| format!("reading {}", mine.display()))?,
            source: Source::Yours,
            path: Some(mine),
        });
    }
    match BUILT_IN.iter().find(|(n, _)| *n == name) {
        Some((n, t)) => {
            Ok(Found { name: n.to_string(), text: t.to_string(), source: Source::BuiltIn, path: None })
        }
        None => bail!("there is no recipe {name}: varde recipe list shows them"),
    }
}

/// A check's rubric or verifier, as kept. It names a file read later,
/// outside; from a session in a sandbox it is kept as the text of a file
/// in its checkout, read now, or as the text given, never as a path.
fn as_text(v: Option<String>) -> Result<Option<String>> {
    let Some(v) = v.filter(|v| !v.trim().is_empty()) else { return Ok(None) };
    let Some(root) = crate::sandbox::bounded_root() else { return flow::rubric_arg(&v).map(Some) };
    let named = Path::new(&v);
    if root.join(named).exists() || v.starts_with('/') || v.contains("..") || v.starts_with('~') {
        return crate::sandbox::read_beneath(&root, named).map(Some).map_err(|_| {
            anyhow::anyhow!("{v} is not a file in this checkout: a session in a sandbox reads no other")
        });
    }
    Ok(Some(v))
}

/// A content hash that is the same on every machine and release (FNV-1a).
fn hash(text: &str) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in text.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{:012x}", h & 0xffff_ffff_ffff)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    label: Option<String>,
    description: Option<String>,
    #[serde(default)]
    vars: BTreeMap<String, Var>,
    #[serde(default)]
    steps: Vec<toml::Table>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Var {
    #[serde(default)]
    pub required: bool,
    pub default: Option<String>,
    pub description: Option<String>,
}

/// One step, resolved: `use` merged, `--set` applied, variables filled.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Step {
    pub id: String,
    pub title: Option<String>,
    pub prompt: Option<String>,
    pub profile: Option<String>,
    #[serde(default)]
    pub needs: Vec<String>,
    /// What steps after it wait on: `verified` (its check passes) or
    /// `approved` (you say so). Default: verified with a check, else approved.
    pub gate: Option<String>,
    pub check: Option<CheckSpec>,
    #[serde(default)]
    pub guard: Vec<String>,
    #[serde(rename = "loop")]
    pub repeat: Option<LoopSpec>,
    /// `own`: a worktree of its own; `upstream`: its one upstream's.
    pub tree: Option<String>,
    /// `branch`, `draft-pr`, `file:<path>` or `report`.
    pub output: Option<String>,
    /// The sandbox its background sessions and checks run in: `default`,
    /// `push`, `off` or yours. Default: `push` for a draft-pr, else `default`.
    pub sandbox: Option<String>,
    /// Read by `resolve`, which merges it before the step is.
    #[serde(rename = "use")]
    #[allow(dead_code)]
    pub uses: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckSpec {
    pub cmd: Option<String>,
    #[serde(default)]
    pub review: bool,
    pub rubric: Option<String>,
    /// What an agent session is asked to verify: a file, or the text.
    pub verify: Option<String>,
    pub reviewer: Option<String>,
    pub again: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoopSpec {
    pub max: Option<i64>,
    pub minutes: Option<i64>,
    #[serde(default)]
    pub escalate: Vec<String>,
}

/// A recipe, ready to make tasks from.
pub struct Recipe {
    pub name: String,
    pub description: Option<String>,
    pub source: Source,
    pub path: Option<PathBuf>,
    pub hash: String,
    pub vars: BTreeMap<String, Var>,
    pub steps: Vec<Step>,
    /// Where each step's keys came from: (step, key) → `built-in`, `yours`,
    /// `from <recipe>`, `--set`.
    pub origin: BTreeMap<(String, String), String>,
}

/// Parse `name` (a recipe's name, or a file), merge what its steps
/// `use`, apply `--set`, and fill in `vars` (with `title` and `context`).
/// `vars` None leaves the `{{…}}` as they are, for `show`.
pub fn resolve(name: &str, sets: &[String], vars: Option<&BTreeMap<String, String>>) -> Result<Recipe> {
    let found = find(name)?;
    let file: File =
        toml::from_str(&found.text).with_context(|| format!("reading the recipe {}", found.name))?;
    let mut origin = BTreeMap::new();
    let mut tables = vec![];
    for (i, mut t) in file.steps.into_iter().enumerate() {
        let id = t
            .get("id")
            .and_then(|v| v.as_str())
            .map(String::from)
            .with_context(|| format!("step {} has no id", i + 1))?;
        for k in t.keys() {
            origin.insert((id.clone(), k.clone()), found.source.name().to_string());
        }
        if let Some(used) = t.get("use").and_then(|v| v.as_str()).map(String::from) {
            if used == found.name {
                bail!("step {id} uses its own recipe");
            }
            let base = find(&used)?;
            let base_file: File =
                toml::from_str(&base.text).with_context(|| format!("reading the recipe {used}"))?;
            let mut merged = base_file
                .steps
                .into_iter()
                .next()
                .with_context(|| format!("the recipe {used} has no step"))?;
            for k in merged.keys() {
                origin.entry((id.clone(), k.clone())).or_insert_with(|| format!("from {used}"));
            }
            merge(&mut merged, std::mem::take(&mut t));
            t = merged;
            t.insert("id".into(), toml::Value::String(id.clone()));
        }
        tables.push(t);
    }
    for s in sets {
        let (path, value) = s.split_once('=').with_context(|| format!("--set {s}: write STEP.KEY=VALUE"))?;
        let mut parts = path.split('.');
        let step = parts.next().unwrap_or("");
        let keys: Vec<&str> = parts.collect();
        if keys.is_empty() {
            bail!("--set {s}: write STEP.KEY=VALUE, e.g. fix.output=draft-pr");
        }
        let t = tables
            .iter_mut()
            .find(|t| t.get("id").and_then(|v| v.as_str()) == Some(step))
            .with_context(|| format!("--set {s}: the recipe has no step {step}"))?;
        // A number, true or a list as TOML writes them; anything else is text.
        let value = toml::from_str::<toml::Table>(&format!("v = {value}"))
            .ok()
            .and_then(|mut t| t.remove("v"))
            .unwrap_or(toml::Value::String(value.into()));
        // A list typed with commas: `fix.loop.escalate=deep,deep`.
        let value = match (&value, keys.last()) {
            (toml::Value::String(v), Some(&("escalate" | "guard" | "needs"))) => {
                toml::Value::Array(v.split(',').map(|x| toml::Value::String(x.trim().into())).collect())
            }
            _ => value,
        };
        set_path(t, &keys, value);
        origin.insert((step.to_string(), keys[0].to_string()), "--set".into());
    }
    let resolved_text = format!("{}{}", found.text, sets.join("\n"));
    let mut all_vars: BTreeMap<String, String> = BTreeMap::new();
    if let Some(given) = vars {
        for (k, v) in &file.vars {
            match (given.get(k), &v.default, v.required) {
                (Some(g), _, _) => {
                    all_vars.insert(k.clone(), g.clone());
                }
                (None, Some(d), false) => {
                    all_vars.insert(k.clone(), d.clone());
                }
                (None, _, _) => bail!(
                    "the recipe {} needs --var {k}=…{}",
                    found.name,
                    v.description.as_ref().map(|d| format!(" ({d})")).unwrap_or_default()
                ),
            }
        }
        for (k, v) in given {
            if !file.vars.contains_key(k) && k != "title" && k != "context" {
                bail!("the recipe {} has no variable {k}", found.name);
            }
            all_vars.insert(k.clone(), v.clone());
        }
    }
    let mut steps = vec![];
    for t in tables {
        let t = if vars.is_some() { fill(toml::Value::Table(t), &all_vars) } else { toml::Value::Table(t) };
        let step: Step = t.try_into().context("a step of the recipe")?;
        steps.push(step);
    }
    let r = Recipe {
        name: found.name,
        description: file.description.or(file.label),
        source: found.source,
        path: found.path,
        hash: hash(&resolved_text),
        vars: file.vars,
        steps,
        origin,
    };
    validate(&r)?;
    Ok(r)
}

/// `b`'s keys over `a`'s, tables merged key by key.
fn merge(a: &mut toml::Table, b: toml::Table) {
    for (k, v) in b {
        match (a.get_mut(&k), v) {
            (Some(toml::Value::Table(x)), toml::Value::Table(y)) => merge(x, y),
            (_, v) => {
                a.insert(k, v);
            }
        }
    }
}

fn set_path(t: &mut toml::Table, keys: &[&str], value: toml::Value) {
    if keys.len() == 1 {
        t.insert(keys[0].into(), value);
        return;
    }
    let inner = t.entry(keys[0].to_string()).or_insert_with(|| toml::Value::Table(toml::Table::new()));
    if !inner.is_table() {
        *inner = toml::Value::Table(toml::Table::new());
    }
    if let toml::Value::Table(inner) = inner {
        set_path(inner, &keys[1..], value);
    }
}

/// Every `{{name}}` in every string, filled in.
fn fill(v: toml::Value, vars: &BTreeMap<String, String>) -> toml::Value {
    match v {
        toml::Value::String(s) => {
            let mut out = s;
            for (k, val) in vars {
                out = out.replace(&format!("{{{{{k}}}}}"), val);
            }
            toml::Value::String(out)
        }
        toml::Value::Array(a) => toml::Value::Array(a.into_iter().map(|x| fill(x, vars)).collect()),
        toml::Value::Table(t) => toml::Value::Table(t.into_iter().map(|(k, x)| (k, fill(x, vars))).collect()),
        other => other,
    }
}

/// What makes a recipe unusable, refused before anything is made.
fn validate(r: &Recipe) -> Result<()> {
    if r.steps.is_empty() {
        bail!("the recipe {} has no steps", r.name);
    }
    let ids: Vec<&str> = r.steps.iter().map(|s| s.id.as_str()).collect();
    for (i, s) in r.steps.iter().enumerate() {
        if ids[..i].contains(&s.id.as_str()) {
            bail!("two steps are called {}", s.id);
        }
        for n in &s.needs {
            if !ids[..i].contains(&n.as_str()) {
                bail!("step {} needs {n}, which is not a step before it", s.id);
            }
        }
        if let Some(g) = &s.gate {
            if g != "verified" && g != "approved" {
                bail!("step {}: gate is verified or approved, not {g}", s.id);
            }
        }
        if let Some(t) = &s.tree {
            if t != "own" && t != "upstream" {
                bail!("step {}: tree is own or upstream, not {t}", s.id);
            }
            if t == "upstream" && s.needs.len() != 1 {
                bail!("step {}: tree = \"upstream\" needs exactly one step it needs", s.id);
            }
        }
        if let Some(o) = &s.output {
            if !(o == "branch" || o == "draft-pr" || o == "report" || o == "trail" || o.starts_with("file:"))
            {
                bail!("step {}: output is branch, draft-pr, report, trail or file:<path>, not {o}", s.id);
            }
        }
        let profiles = s
            .profile
            .iter()
            .chain(s.repeat.iter().flat_map(|l| l.escalate.iter()))
            .chain(s.check.iter().filter_map(|c| c.reviewer.as_ref()));
        for p in profiles {
            let p = p.split(':').next().unwrap_or(p);
            if !p.contains("{{") && config::get().profile(p).is_none() {
                bail!("step {}: there is no profile {p}", s.id);
            }
        }
        if let Some(b) = s.sandbox.as_deref().filter(|b| !b.contains("{{")) {
            crate::sandbox::policy(Some(b)).with_context(|| format!("step {}", s.id))?;
        }
        if s.repeat.is_some() && s.check.is_none() {
            bail!("step {} loops but has no check to loop until", s.id);
        }
    }
    Ok(())
}

/// The file a step whose output is a trail writes.
pub const TRAIL: &str = "trail.toml";

/// What a step's output asks of its session.
fn output_words(output: &str) -> Option<String> {
    Some(match output {
        "branch" => return None,
        "draft-pr" => "When the work is done and its check passes, push this branch and open a draft pull request \
                       (gh pr create --draft). If you may not push, end by giving the user the exact commands to run."
            .into(),
        "report" => "Change no files. Your last message is the result: write it as a report for the user.".into(),
        "trail" => format!(
            "Write the plan as a varde recipe in {TRAIL} at the root of this checkout: run `varde recipe guide` for \
             the format, and `varde recipe check {TRAIL}` until it is usable. Make each step one piece of the work \
             that a fresh session can build and check on its own, with its own check command; steps that do not \
             depend on each other run at the same time, each in its own worktree. End with a step that needs them \
             all, merges their branches and runs the whole check. Give steps that run at the same time files of \
             their own, so the merge does not conflict; write no branch names or merging into a step's prompt \
             (varde starts each step from what it needs and names the branches to merge). Write each command \
             out in full: a trail takes no \
             variables but {{{{title}}}} and {{{{context}}}}. Two to six steps; a check, not the user's approval, \
             between them, unless only the user can judge. Commit it. When the user approves this task, \
             varde makes the tasks from {TRAIL}, after this one, and sets them going."
        ),
        f => format!(
            "Write the result to {} in this directory, making any folders it needs, and say its path at the end.",
            f.strip_prefix("file:")?
        ),
    })
}

/// The gate a step's dependents wait on.
fn gate(s: &Step) -> &'static str {
    match s.gate.as_deref() {
        Some("approved") => "approved",
        Some(_) => "verified",
        None if s.check.is_some() => "verified",
        None => "approved",
    }
}

/// `varde new TITLE --recipe NAME`: one task per step, joined as the
/// recipe says, each with its profile, check and loop. Nothing starts
/// until the trail is set going (`--start`, or `varde trail <root> --start`).
#[allow(clippy::too_many_arguments)]
pub fn new(
    paths: &Paths,
    name: &str,
    title: &[String],
    project_dir: Option<PathBuf>,
    body: Option<String>,
    vars: &[String],
    sets: &[String],
    profile: Option<String>,
    after: (&[String], bool),
    go: bool,
    as_json: bool,
) -> Result<()> {
    let store = paths.open(BUSY)?;
    if project_dir.is_some() {
        crate::sandbox::not_inside("Making tasks in another project")?;
    }
    if go {
        crate::sandbox::not_inside("Setting a trail going")?;
    }
    let (after, approved) = after;
    let after = after.iter().map(|a| task::resolve(&store, a)).collect::<Result<Vec<_>>>()?;
    for a in &after {
        crate::sandbox::own_project(&store, a)?;
    }
    let title = title.join(" ").trim().to_string();
    if title.is_empty() {
        bail!("a task made from a recipe needs a title");
    }
    let body = crate::arg_or_stdin(body)?.map(|b| b.trim().to_string()).unwrap_or_default();
    let root = match project_dir {
        Some(d) => crate::start::project_of(&store, &d)?,
        None => project::of(&store.conn, &std::env::current_dir().context("current directory")?),
    };
    let repo = config::Repo::read(&root).unwrap_or_default();
    let mut given: BTreeMap<String, String> = BTreeMap::new();
    for v in vars {
        let (k, val) = v.split_once('=').with_context(|| format!("--var {v}: write NAME=VALUE"))?;
        given.insert(k.trim().into(), val.into());
    }
    given.insert("title".into(), title.clone());
    given.insert("context".into(), body.clone());
    // A check named nowhere falls back on the repository's, for a recipe
    // that takes one.
    let takes_check = resolve(name, sets, None)?.vars.contains_key("check");
    if takes_check && !given.contains_key("check") {
        if let Some(c) = &repo.check {
            given.insert("check".into(), c.clone());
        }
    }
    let r = resolve(name, sets, Some(&given))?;
    if let Some(p) = &profile {
        if config::get().profile(p).is_none() {
            bail!("there is no profile {p}");
        }
    }
    for s in &r.steps {
        // A command the recipe names that came out empty: a review alone
        // in its place would loop on judgement, which is not what it said.
        if s.check.as_ref().is_some_and(|c| c.cmd.as_deref().is_some_and(|c| c.trim().is_empty())) {
            bail!(
                "step {} has no check command: --var check=CMD, or git config varde.check in the repository",
                s.id
            );
        }
    }
    let mut warned = vec![];
    for p in
        r.steps.iter().flat_map(|s| s.profile.iter().chain(s.repeat.iter().flat_map(|l| l.escalate.iter())))
    {
        let p = p.split(':').next().unwrap_or(p).to_string();
        if !warned.contains(&p) {
            if let Some(w) = config::get().profile_warning(&p) {
                eprintln!("varde: {w}");
            }
            warned.push(p);
        }
    }
    let now = now_ms();
    let tx = rusqlite::Transaction::new_unchecked(&store.conn, rusqlite::TransactionBehavior::Immediate)?;
    let mut made: Vec<(String, String)> = vec![];
    let single = r.steps.len() == 1;
    for s in &r.steps {
        let t_title = s.title.clone().unwrap_or_else(|| {
            if single {
                title.clone()
            } else {
                format!("{title} · {}", s.id)
            }
        });
        let mut b = s.prompt.clone().unwrap_or_else(|| body.clone()).trim().to_string();
        if let Some(words) = s.output.as_deref().and_then(output_words) {
            b.push_str(&format!("\n\n{words}"));
        }
        let id =
            task::create(&store, &t_title, Some(b.trim()), Some(&root.display().to_string()), None, now)?;
        let prof = s.profile.clone().or_else(|| profile.clone());
        let sandbox = crate::sandbox::for_new(step_sandbox(s))?;
        store.conn.execute(
            "UPDATE task SET profile = ?2, recipe = ?3, inbox = 0, sandbox = ?4, output = ?5 WHERE id = ?1",
            params![id, prof, format!("{}@{}", r.name, r.hash), sandbox, s.output],
        )?;
        // What the trail waits on, before its first steps.
        if s.needs.is_empty() {
            for a in &after {
                flow::add_edge(&store, &id, a, approved)?;
            }
        }
        if let Some(c) = &s.check {
            let cmd = c.cmd.clone().filter(|c| !c.trim().is_empty());
            let rubric = as_text(c.rubric.clone()).with_context(|| format!("step {}", s.id))?;
            let verify = as_text(c.verify.clone()).with_context(|| format!("step {}", s.id))?;
            flow::set_def(
                &store,
                &id,
                &flow::Def {
                    review: c.review || (cmd.is_none() && rubric.is_some()),
                    cmd,
                    rubric,
                    reviewer: c.reviewer.clone(),
                    verify,
                    guard: s.guard.clone(),
                    again: c.again.unwrap_or(1),
                    base_tree: None,
                },
            )?;
        }
        if let Some(l) = &s.repeat {
            store.conn.execute(
                "INSERT INTO loop (task, max, minutes, escalate, started, paused) VALUES (?1, ?2, ?3, ?4, ?5, 0)",
                params![
                    id,
                    l.max.unwrap_or(3).max(1),
                    l.minutes,
                    (!l.escalate.is_empty()).then(|| serde_json::to_string(&l.escalate).unwrap_or_default()),
                    now
                ],
            )?;
        }
        for n in &s.needs {
            let up_step = r.steps.iter().find(|x| &x.id == n).context("a step it needs")?;
            let up = &made.iter().find(|(sid, _)| sid == n).context("a step made before")?.1;
            flow::add_edge(&store, &id, up, gate(up_step) == "approved")?;
        }
        // One upstream that nothing else follows: work on in its tree.
        let shares = match s.tree.as_deref() {
            Some("upstream") => true,
            Some(_) => false,
            None => {
                s.needs.len() == 1 && r.steps.iter().filter(|x| x.needs.contains(&s.needs[0])).count() == 1
            }
        };
        if shares {
            let up = &made.iter().find(|(sid, _)| *sid == s.needs[0]).context("a step made before")?.1;
            store.conn.execute("UPDATE task SET share = ?2 WHERE id = ?1", params![id, up])?;
        }
        made.push((s.id.clone(), id));
    }
    tx.commit()?;
    let root_id = made[0].1.clone();
    if as_json {
        let tasks: Vec<_> = made
            .iter()
            .map(|(s, id)| serde_json::json!({"step": s, "id": id, "short": task::short(id)}))
            .collect();
        println!("{}", serde_json::json!({"v": 1, "recipe": r.name, "hash": r.hash, "tasks": tasks}));
    }
    drop(store);
    if go {
        if as_json {
            let store = paths.open(BUSY)?;
            for (_, id) in &made {
                store.conn.execute("UPDATE task SET auto = 1 WHERE id = ?1", [id])?;
            }
            drop(store);
            return crate::schedule::tick(paths);
        }
        return flow::trail(paths, &root_id, true, "text", false);
    }
    if !as_json {
        flow::trail(paths, &root_id, false, "text", false)?;
        let short = task::short(&root_id);
        println!("\nmade from {} ({}); varde trail {short} --start sets it going", r.name, r.hash);
    }
    Ok(())
}

/// `varde recipe list`.
pub fn list(as_json: bool) -> Result<()> {
    let mut names: Vec<(String, Source)> =
        BUILT_IN.iter().map(|(n, _)| (n.to_string(), Source::BuiltIn)).collect();
    if let Ok(entries) = std::fs::read_dir(dir()) {
        for e in entries.flatten() {
            let p = e.path();
            if p.extension().is_some_and(|x| x == "toml") {
                let n = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                match names.iter_mut().find(|(x, _)| *x == n) {
                    Some(entry) => entry.1 = Source::Yours,
                    None => names.push((n, Source::Yours)),
                }
            }
        }
    }
    if as_json {
        let all: Vec<_> = names
            .iter()
            .map(|(n, _)| match resolve(n, &[], None) {
                Ok(r) => serde_json::json!({
                    "name": n, "source": r.source.name(), "description": r.description, "hash": r.hash,
                    "path": r.path, "steps": r.steps.iter().map(|s| s.id.clone()).collect::<Vec<_>>(),
                    "vars": r.vars.iter().map(|(k, v)| serde_json::json!({
                        "name": k, "required": v.required, "default": v.default, "description": v.description,
                    })).collect::<Vec<_>>(),
                }),
                Err(e) => serde_json::json!({"name": n, "source": "yours", "error": format!("{e:#}")}),
            })
            .collect();
        println!("{}", serde_json::json!({"v": 1, "recipes": all, "dir": dir()}));
        return Ok(());
    }
    for (n, _) in &names {
        match resolve(n, &[], None) {
            Ok(r) => println!("{:<16} {:<9} {}", n, r.source.name(), r.description.unwrap_or_default()),
            Err(e) => println!("{n:<16} {:<9} broken: {e:#}", "yours"),
        }
    }
    println!("\nvarde recipe show NAME draws one; varde recipe guide says how to write one");
    Ok(())
}

/// The sandbox a step names, or the one its output needs: a draft pull
/// request is pushed, and pushing needs git's logins.
fn step_sandbox(s: &Step) -> Option<&str> {
    s.sandbox.as_deref().or((s.output.as_deref() == Some("draft-pr")).then_some("push"))
}

/// One line for a step, as `show` and `check` draw it.
fn step_line(s: &Step) -> String {
    let mut parts = vec![s.id.clone()];
    parts.push(s.profile.clone().unwrap_or_else(|| "work".into()));
    if let Some(c) = &s.check {
        let mut what = vec![];
        if let Some(cmd) = c.cmd.as_deref().filter(|c| !c.is_empty()) {
            what.push(format!("`{cmd}`"));
        }
        if c.review {
            what.push(format!(
                "review{}",
                c.reviewer.as_ref().map(|r| format!(" by {r}")).unwrap_or_default()
            ));
        }
        if c.verify.as_ref().is_some_and(|v| !v.trim().is_empty()) {
            what.push("an agent verifies".into());
        }
        if c.again.is_some_and(|a| a > 1) {
            what.push(format!("{} in a row", c.again.unwrap_or(1)));
        }
        parts.push(format!("check {}", what.join(" + ")));
    }
    if let Some(l) = &s.repeat {
        let esc =
            if l.escalate.is_empty() { String::new() } else { format!(" → {}", l.escalate.join(" → ")) };
        parts.push(format!("↺ max {}{esc}", l.max.unwrap_or(3)));
    }
    if !s.guard.is_empty() {
        parts.push(format!("guard {}", s.guard.join(" ")));
    }
    if let Some(o) = &s.output {
        parts.push(format!("output {o}"));
    }
    if let Some(b) = step_sandbox(s) {
        parts.push(format!("sandbox {b}"));
    }
    parts.join(" · ")
}

/// The steps drawn as `varde trail` draws tasks.
fn drawn(r: &Recipe, origin: bool) -> Vec<String> {
    let lines: Vec<(String, String)> = r
        .steps
        .iter()
        .map(|s| {
            let mut text = step_line(s);
            if origin {
                let mut from: Vec<String> = r
                    .origin
                    .iter()
                    .filter(|((id, k), _)| id == &s.id && k != "id")
                    .map(|((_, k), o)| format!("{k}: {o}"))
                    .collect();
                from.sort();
                text.push_str(&format!("   [{}]", from.join(", ")));
            }
            (s.id.clone(), text)
        })
        .collect();
    let edges: Vec<(String, String, String)> = r
        .steps
        .iter()
        .flat_map(|s| {
            s.needs.iter().map(move |n| {
                let up = r.steps.iter().find(|x| &x.id == n).map_or("verified", gate);
                (s.id.clone(), n.clone(), up.to_string())
            })
        })
        .collect();
    flow::draw(&lines, &edges, &|_| String::new()).into_iter().map(|l| l.trim_end().to_string()).collect()
}

/// `varde recipe show NAME`.
pub fn show(name: &str, sets: &[String], origin: bool, format: &str) -> Result<()> {
    let r = resolve(name, sets, None)?;
    if format == "mermaid" {
        let lines: Vec<(String, String)> = r.steps.iter().map(|s| (s.id.clone(), step_line(s))).collect();
        let edges: Vec<(String, String, String)> = r
            .steps
            .iter()
            .flat_map(|s| s.needs.iter().map(move |n| (s.id.clone(), n.clone(), "verified".to_string())))
            .collect();
        println!("{}", flow::mermaid(&lines, &edges, &|id| id.to_string()));
        return Ok(());
    }
    let from = match (&r.source, &r.path) {
        (Source::BuiltIn, _) => "built-in".to_string(),
        (_, Some(p)) => p.display().to_string(),
        _ => r.source.name().into(),
    };
    println!("{} · {} ({})", r.name, from, r.hash);
    if let Some(d) = &r.description {
        println!("{d}");
    }
    if !r.vars.is_empty() {
        let vars: Vec<String> = r
            .vars
            .iter()
            .map(|(k, v)| match (&v.default, v.required) {
                (_, true) | (None, _) => format!("{k} (required)"),
                (Some(d), false) if d.is_empty() => k.to_string(),
                (Some(d), false) => format!("{k}={d}"),
            })
            .collect();
        println!("vars: {}", vars.join(", "));
    }
    println!();
    for l in drawn(&r, origin) {
        println!("{l}");
    }
    Ok(())
}

/// `varde recipe check FILE`: whether a recipe is usable, and how it draws.
pub fn check(path: &str) -> Result<()> {
    let r = resolve(path, &[], None)?;
    println!("{} is a usable recipe ({}):\n", path, r.hash);
    for l in drawn(&r, false) {
        println!("{l}");
    }
    let mine = dir().join(format!("{}.toml", r.name));
    if Path::new(path).canonicalize().ok() != mine.canonicalize().ok() {
        println!("\nto use it by name: cp {path} {}", mine.display());
    }
    Ok(())
}

/// `varde recipe edit NAME`: your copy of it, made from the built-in the
/// first time, in $EDITOR.
pub fn edit(name: &str) -> Result<()> {
    let path = dir().join(format!("{name}.toml"));
    if !path.is_file() {
        let text = match BUILT_IN.iter().find(|(n, _)| *n == name) {
            Some((_, t)) => t.to_string(),
            None => format!(
                "# {name}: see `varde recipe guide`.\nlabel = \"{name}\"\ndescription = \"\"\n\n[[steps]]\nid = \"work\"\nprompt = \"{{{{context}}}}\"\ncheck = {{ cmd = \"\" }}\nloop = {{ max = 3 }}\n"
            ),
        };
        std::fs::create_dir_all(dir())?;
        std::fs::write(&path, text)?;
        println!("made {}", path.display());
    }
    if unsafe { libc::isatty(0) } != 1 {
        println!("{}", path.display());
        return Ok(());
    }
    let editor = std::env::var("VISUAL").or_else(|_| std::env::var("EDITOR")).unwrap_or_else(|_| "vi".into());
    let status = std::process::Command::new("sh")
        .args(["-c", &format!("{editor} \"$1\""), "sh"])
        .arg(&path)
        .status()?;
    if !status.success() {
        bail!("{editor} exited with {status}");
    }
    match resolve(&path.display().to_string(), &[], None) {
        Ok(r) => println!("{} saved ({})", r.name, r.hash),
        Err(e) => println!("saved, but it is not usable yet: {e:#}"),
    }
    Ok(())
}

/// `varde recipe guide`: how to write one, for you and for an agent.
pub fn guide() -> Result<()> {
    print!("{}", GUIDE.replace("{dir}", &dir().display().to_string()));
    Ok(())
}

const GUIDE: &str = r#"Writing a varde recipe

A recipe is a loop or a trail written down once: TOML steps, each one varde
task with its own profile, prompt, check, loop, guard and output, joined by
`needs`. varde runs it: every round of a loop is a fresh background session
given the task's cairn and how the last check failed; a step starts when
what it needs is verified (its check passed) or approved (the user said so).
Nothing merges into the base, and nothing lands by itself.

Save it as {dir}/NAME.toml (yours shadows a built-in of the same name), then
  varde recipe check FILE              is it usable? draws it
  varde recipe show NAME [--origin]    draws it, and where each value came from
  varde new "TITLE" --recipe NAME [--var k=v] [--set step.key=v] [--start]

The file

  label = "bugfix"                      # a name for people
  description = "one line, shown by varde recipe list"

  [vars.check]                          # {{check}} in any string below
  description = "what it is for"
  default = ""                          # leave out, or required = true, to insist

  [[steps]]                             # one per task, in order
  id = "fix"                            # unique; what other steps' needs name
  title = "Fix: {{title}}"              # default: TITLE · id (TITLE alone for one step)
  prompt = """..."""                    # what the session is asked; default {{context}}
  profile = "build"                     # quick | build | deep | review | yours; default the work role
  needs = ["repro"]                     # steps before it; default none
  gate = "verified"                     # what steps after it wait on: verified | approved
                                        #   default: verified with a check, else approved
  check = { cmd = "cargo test", review = true, rubric = "FILE or text", reviewer = "review", again = 1 }
                                        # cmd: exits 0 to pass; review: another model reads the
                                        #   diff after cmd passes (or alone); again: N passes in a row
                                        # verify = "FILE or text": then an agent session (the reviewer
                                        #   profile, its tools and skills, in the sandbox) checks the
                                        #   work as asked ("run it and use /verify-app"): PASS or FAIL
  loop = { max = 3, minutes = 60, escalate = ["deep", "deep:xhigh"] }
                                        # fresh sessions until the check passes; needs a check
  guard = ["tests/*"]                   # paths a round must not change: it is flagged, the loop stops
  tree = "upstream"                     # own | upstream (its one upstream's worktree)
                                        #   default: upstream when it needs one step nothing else needs
                                        #   own: cut from its one upstream's branch; with several,
                                        #   varde names their branches and the step merges them
  output = "branch"                     # branch | draft-pr | file:PATH | report | trail
                                        #   trail: it writes trail.toml, a recipe of its own,
                                        #   whose tasks varde makes after it once you approve it
  sandbox = "default"                   # what its background sessions reach: default | push |
                                        #   off | yours ([sandboxes.NAME]); varde sandbox shows them
                                        #   default: push for a draft-pr, else default
  use = "fix-until-green"               # start from that recipe's first step; keys here win

{{title}} and {{context}} are the title and --context given to varde new.
--set step.key=value changes one key for one task: fix.output=draft-pr,
fix.loop.max=5, fix.loop.escalate=deep,deep:xhigh.

Rules of thumb
- Few steps: 1 to 4 (a trail a plan writes: 2 to 6). A step is a whole session;
  steps that run at the same time need files of their own.
- Every step that loops has a check, and a command beats a review: a review
  alone gets two rounds and then the user.
- Guard what the check reads (tests/*) when a step must not change it.
- Put the user where only they can judge: gate = "approved" on a spec or a repro.
- No branch names or merge steps in prompts: varde starts a step from what it
  needs, tells it every upstream's branch, and asks a step of several to merge.
- Any logic varde has no word for goes in the check's command or the prompt.
- Skills are the how: name one in a prompt (`/spec`) or in a check's verify
  ("use /verify-app"), and every round runs it.
- What must not happen inside a loop (an apply, a deploy) goes outside it: a
  step in a sandbox that denies it writes the commands into a file:CHECKLIST.md
  output, gated approved, and the user runs them.

Built-in: fix-until-green, bugfix, feature, epic, change-plan, draft-revise,
research (varde recipe show NAME prints each).
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_built_in_resolves_and_draws() {
        for (name, _) in BUILT_IN {
            let r = resolve(name, &[], None).unwrap_or_else(|e| panic!("{name}: {e:#}"));
            assert!(!drawn(&r, true).is_empty());
        }
    }

    #[test]
    fn use_merges_and_set_wins() {
        let r = resolve("bugfix", &["fix.output=draft-pr".into(), "fix.loop.max=5".into()], None).unwrap();
        let fix = r.steps.iter().find(|s| s.id == "fix").unwrap();
        // From fix-until-green: the loop; from bugfix: the review.
        assert_eq!(fix.repeat.as_ref().unwrap().max, Some(5));
        assert_eq!(fix.repeat.as_ref().unwrap().escalate, vec!["deep".to_string()]);
        assert!(fix.check.as_ref().unwrap().review);
        assert_eq!(fix.output.as_deref(), Some("draft-pr"));
        assert_eq!(r.origin.get(&("fix".into(), "output".into())).map(String::as_str), Some("--set"));
        assert_eq!(r.origin.get(&("fix".into(), "loop".into())).map(String::as_str), Some("--set"));
        assert_eq!(r.origin.get(&("fix".into(), "guard".into())).map(String::as_str), Some("built-in"));
    }

    #[test]
    fn vars_fill_and_a_missing_one_is_named() {
        let mut v = BTreeMap::new();
        v.insert("title".to_string(), "Parser crash".to_string());
        v.insert("context".to_string(), "".to_string());
        v.insert("check".to_string(), "cargo test".to_string());
        let r = resolve("bugfix", &[], Some(&v)).unwrap();
        let fix = r.steps.iter().find(|s| s.id == "fix").unwrap();
        assert_eq!(fix.check.as_ref().unwrap().cmd.as_deref(), Some("cargo test"));
        assert_eq!(fix.title.as_deref(), Some("Fix: Parser crash"));
        assert_eq!(fix.guard, vec!["tests/*".to_string()]);
    }

    #[test]
    fn a_broken_recipe_says_why() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.toml");
        std::fs::write(&p, "[[steps]]\nid = \"a\"\nneeds = [\"b\"]\n").unwrap();
        let e = resolve(&p.display().to_string(), &[], None).err().unwrap();
        assert!(format!("{e:#}").contains("needs b"), "{e:#}");
        std::fs::write(&p, "[[steps]]\nid = \"a\"\nloop = { max = 2 }\n").unwrap();
        let e = resolve(&p.display().to_string(), &[], None).err().unwrap();
        assert!(format!("{e:#}").contains("no check"), "{e:#}");
    }

    #[test]
    fn the_hash_is_stable() {
        assert_eq!(hash("abc"), hash("abc"));
        assert_ne!(hash("abc"), hash("abd"));
        assert_eq!(hash("").len(), 12);
    }
}
