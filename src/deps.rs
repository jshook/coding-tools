// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! The `deps` built-in check's crate-graph queries.
//!
//! The resolved dependency graph comes from `cargo metadata --format-version
//! 1 --locked --offline` (hermetic by construction); this module parses that
//! JSON into a [`Graph`] and answers the crate-level invariant questions:
//! *is crate X anywhere in the tree* ([`deny_paths`]), *does workspace member
//! A reach crate B* ([`forbid_path`]), *do any crates resolve at more than one
//! version* ([`Graph::duplicates`]), *is the graph free of cycles* ([`cycles`]),
//! and *does it respect a declared layer order* ([`layer_violations`], with
//! membership from [`assign_layers`]). Every violation carries its evidence —
//! a dependency path, a concrete cycle, or the duplicated version list — so a
//! red answer is never just a name.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use clap::{CommandFactory, Parser};

use crate::rules::ProbeOutcome;
use crate::{pattern, supervise};

/// A dependency edge kind, as cargo models it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, clap::ValueEnum)]
pub enum EdgeKind {
    /// Ordinary `[dependencies]`.
    Normal,
    /// `[build-dependencies]`.
    Build,
    /// `[dev-dependencies]`.
    Dev,
}

impl EdgeKind {
    fn from_metadata(kind: &serde_json::Value) -> Option<EdgeKind> {
        match kind.as_str() {
            None => Some(EdgeKind::Normal), // null == normal in cargo metadata
            Some("build") => Some(EdgeKind::Build),
            Some("dev") => Some(EdgeKind::Dev),
            Some(_) => None,
        }
    }
}

/// One resolved package.
#[derive(Debug, Clone)]
pub struct Package {
    /// Crate name.
    pub name: String,
    /// Resolved version.
    pub version: String,
}

/// The resolved crate graph.
pub struct Graph {
    /// Package id → name/version.
    pub packages: HashMap<String, Package>,
    /// Package id → outgoing edges (dependency package id, edge kinds).
    pub edges: HashMap<String, Vec<(String, Vec<EdgeKind>)>>,
    /// Workspace member package ids.
    pub members: Vec<String>,
}

/// Parse `cargo metadata --format-version 1` JSON into a [`Graph`].
pub fn parse_metadata(text: &str) -> Result<Graph, String> {
    let v: serde_json::Value =
        serde_json::from_str(text).map_err(|e| format!("cargo metadata JSON: {e}"))?;
    let mut packages = HashMap::new();
    for p in v["packages"].as_array().ok_or("metadata missing packages")? {
        let id = p["id"].as_str().ok_or("package missing id")?.to_string();
        packages.insert(
            id,
            Package {
                name: p["name"].as_str().unwrap_or("").to_string(),
                version: p["version"].as_str().unwrap_or("").to_string(),
            },
        );
    }
    let members: Vec<String> = v["workspace_members"]
        .as_array()
        .ok_or("metadata missing workspace_members")?
        .iter()
        .filter_map(|m| m.as_str().map(String::from))
        .collect();
    let mut edges: HashMap<String, Vec<(String, Vec<EdgeKind>)>> = HashMap::new();
    let nodes = v["resolve"]["nodes"]
        .as_array()
        .ok_or("metadata missing resolve.nodes (was --no-deps used?)")?;
    for node in nodes {
        let id = node["id"].as_str().ok_or("node missing id")?.to_string();
        let mut out = Vec::new();
        for dep in node["deps"].as_array().unwrap_or(&Vec::new()) {
            let pkg = dep["pkg"].as_str().unwrap_or("").to_string();
            let kinds: Vec<EdgeKind> = dep["dep_kinds"]
                .as_array()
                .map(|ks| {
                    ks.iter()
                        .filter_map(|k| EdgeKind::from_metadata(&k["kind"]))
                        .collect()
                })
                .unwrap_or_else(|| vec![EdgeKind::Normal]);
            out.push((pkg, kinds));
        }
        edges.insert(id, out);
    }
    Ok(Graph {
        packages,
        edges,
        members,
    })
}

impl Graph {
    /// Package ids whose crate name is exactly `name`.
    pub fn ids_named(&self, name: &str) -> Vec<&str> {
        // Only ids present in the resolve graph count as "in the tree".
        let mut ids: Vec<&str> = self
            .edges
            .keys()
            .filter(|id| self.packages.get(*id).is_some_and(|p| p.name == name))
            .map(String::as_str)
            .collect();
        ids.sort();
        ids
    }

    fn label(&self, id: &str) -> String {
        match self.packages.get(id) {
            // A versionless node (the module graph) labels by name alone.
            Some(p) if p.version.is_empty() => p.name.clone(),
            Some(p) => format!("{} v{}", p.name, p.version),
            None => id.to_string(),
        }
    }

    /// The dependency ids reachable from `node` over `allowed` edges, sorted
    /// and de-duplicated for deterministic traversal. When `restrict` is set,
    /// only ids within that node set are returned (the induced subgraph).
    fn successors<'a>(
        &'a self,
        node: &str,
        allowed: &HashSet<EdgeKind>,
        restrict: Option<&HashSet<&str>>,
    ) -> Vec<&'a str> {
        let mut s: Vec<&str> = self
            .edges
            .get(node)
            .map(Vec::as_slice)
            .unwrap_or(&[])
            .iter()
            .filter(|(_, kinds)| kinds.iter().any(|k| allowed.contains(k)))
            .map(|(dep, _)| dep.as_str())
            .filter(|dep| restrict.is_none_or(|r| r.contains(*dep)))
            .collect();
        s.sort_unstable();
        s.dedup();
        s
    }

    /// BFS from `starts` to the first node satisfying `is_target` (the starts
    /// themselves never count), traversing only `allowed` edge kinds. Returns
    /// the evidence path as `name vX` labels.
    fn bfs_path(
        &self,
        starts: &[&str],
        is_target: impl Fn(&str) -> bool,
        allowed: &HashSet<EdgeKind>,
    ) -> Option<Vec<String>> {
        let mut parent: HashMap<&str, &str> = HashMap::new();
        let mut queue: VecDeque<&str> = VecDeque::new();
        let mut seen: HashSet<&str> = HashSet::new();
        for s in starts {
            seen.insert(s);
            queue.push_back(s);
        }
        while let Some(id) = queue.pop_front() {
            if !starts.contains(&id) && is_target(id) {
                let mut path = vec![id];
                let mut cur = id;
                while let Some(&p) = parent.get(cur) {
                    path.push(p);
                    cur = p;
                }
                path.reverse();
                return Some(path.iter().map(|i| self.label(i)).collect());
            }
            for (dep, kinds) in self.edges.get(id).map(Vec::as_slice).unwrap_or(&[]) {
                if !kinds.iter().any(|k| allowed.contains(k)) {
                    continue;
                }
                let dep: &str = dep.as_str();
                if seen.insert(dep) {
                    parent.insert(dep, id);
                    queue.push_back(dep);
                }
            }
        }
        None
    }

    /// BFS from `starts` to the first package named `target`, traversing only
    /// `allowed` edge kinds. Returns the evidence path as `name vX` labels.
    pub fn path_to(
        &self,
        starts: &[&str],
        target: &str,
        allowed: &HashSet<EdgeKind>,
    ) -> Option<Vec<String>> {
        self.bfs_path(
            starts,
            |id| self.packages.get(id).is_some_and(|p| p.name == target),
            allowed,
        )
    }

    /// BFS from `starts` to the first package whose id is in `targets`.
    fn path_to_ids(
        &self,
        starts: &[&str],
        targets: &HashSet<&str>,
        allowed: &HashSet<EdgeKind>,
    ) -> Option<Vec<String>> {
        self.bfs_path(starts, |id| targets.contains(id), allowed)
    }

    /// Tarjan's strongly-connected components over `allowed` edges, each as a
    /// list of package ids. Iterative, so a deep graph cannot overflow the
    /// stack; node iteration is sorted, so the output is deterministic. When
    /// `restrict` is set, only nodes in that set (and edges between them) are
    /// considered — the workspace-member-induced subgraph.
    fn strongly_connected<'a>(
        &'a self,
        allowed: &HashSet<EdgeKind>,
        restrict: Option<&HashSet<&str>>,
    ) -> Vec<Vec<String>> {
        struct Frame<'f> {
            node: &'f str,
            succ: Vec<&'f str>,
            next: usize,
        }
        let mut index: HashMap<&str, usize> = HashMap::new();
        let mut low: HashMap<&str, usize> = HashMap::new();
        let mut on_stack: HashSet<&str> = HashSet::new();
        let mut stack: Vec<&str> = Vec::new();
        let mut counter = 0usize;
        let mut sccs: Vec<Vec<String>> = Vec::new();

        let mut roots: Vec<&str> = self
            .edges
            .keys()
            .map(String::as_str)
            .filter(|id| restrict.is_none_or(|r| r.contains(*id)))
            .collect();
        roots.sort_unstable();

        for root in roots {
            if index.contains_key(root) {
                continue;
            }
            index.insert(root, counter);
            low.insert(root, counter);
            counter += 1;
            stack.push(root);
            on_stack.insert(root);
            let mut work: Vec<Frame<'a>> = vec![Frame {
                node: root,
                succ: self.successors(root, allowed, restrict),
                next: 0,
            }];
            while let Some(top) = work.len().checked_sub(1) {
                let (node, next, len) = {
                    let f = &work[top];
                    (f.node, f.next, f.succ.len())
                };
                if next < len {
                    let w = work[top].succ[next];
                    work[top].next += 1;
                    if !index.contains_key(w) {
                        index.insert(w, counter);
                        low.insert(w, counter);
                        counter += 1;
                        stack.push(w);
                        on_stack.insert(w);
                        let succ = self.successors(w, allowed, restrict);
                        work.push(Frame { node: w, succ, next: 0 });
                    } else if on_stack.contains(w) {
                        let lw = index[w];
                        let e = low.get_mut(node).unwrap();
                        *e = (*e).min(lw);
                    }
                } else {
                    let node_low = low[node];
                    if node_low == index[node] {
                        let mut comp = Vec::new();
                        loop {
                            let w = stack.pop().unwrap();
                            on_stack.remove(w);
                            comp.push(w.to_string());
                            if w == node {
                                break;
                            }
                        }
                        sccs.push(comp);
                    }
                    work.pop();
                    if let Some(parent) = work.last() {
                        let p = parent.node;
                        let e = low.get_mut(p).unwrap();
                        *e = (*e).min(node_low);
                    }
                }
            }
        }
        sccs
    }

    /// A concrete shortest cycle through `start`, restricted to ids in `scc`
    /// and `allowed` edges, as `name vX` labels that close back to `start`.
    fn shortest_cycle_through(
        &self,
        start: &str,
        scc: &HashSet<&str>,
        allowed: &HashSet<EdgeKind>,
    ) -> Vec<String> {
        let neighbors = |id: &str| -> Vec<String> {
            self.edges
                .get(id)
                .map(Vec::as_slice)
                .unwrap_or(&[])
                .iter()
                .filter(|(dep, kinds)| {
                    scc.contains(dep.as_str()) && kinds.iter().any(|k| allowed.contains(k))
                })
                .map(|(dep, _)| dep.clone())
                .collect()
        };
        if neighbors(start).iter().any(|n| n == start) {
            return vec![self.label(start), self.label(start)];
        }
        let mut parent: HashMap<String, String> = HashMap::new();
        let mut seen: HashSet<String> = HashSet::new();
        let mut queue: VecDeque<String> = VecDeque::new();
        seen.insert(start.to_string());
        for n in neighbors(start) {
            if seen.insert(n.clone()) {
                parent.insert(n.clone(), start.to_string());
                queue.push_back(n);
            }
        }
        while let Some(u) = queue.pop_front() {
            for n in neighbors(&u) {
                if n == start {
                    let mut chain = vec![u.clone()];
                    let mut cur = u.clone();
                    while let Some(p) = parent.get(&cur) {
                        chain.push(p.clone());
                        cur = p.clone();
                    }
                    chain.reverse();
                    chain.push(start.to_string());
                    return chain.iter().map(|i| self.label(i)).collect();
                }
                if seen.insert(n.clone()) {
                    parent.insert(n.clone(), u.clone());
                    queue.push_back(n);
                }
            }
        }
        vec![self.label(start)]
    }

    /// Crate names that resolve at more than one version, with the versions.
    pub fn duplicates(&self) -> Vec<(String, Vec<String>)> {
        let mut by_name: BTreeMap<&str, HashSet<&str>> = BTreeMap::new();
        for id in self.edges.keys() {
            if let Some(p) = self.packages.get(id) {
                by_name.entry(&p.name).or_default().insert(&p.version);
            }
        }
        by_name
            .into_iter()
            .filter(|(_, versions)| versions.len() > 1)
            .map(|(name, versions)| {
                let mut v: Vec<String> = versions.into_iter().map(String::from).collect();
                v.sort();
                (name.to_string(), v)
            })
            .collect()
    }
}

/// Evidence for one violated assertion.
#[derive(Debug)]
pub struct Violation {
    /// Which assertion fired: `deny`, `forbid`, or `duplicates`.
    pub check: String,
    /// The offending crate (or `A=>B` spec).
    pub subject: String,
    /// Human evidence: a dependency path, or the duplicated versions.
    pub evidence: String,
}

/// Evaluate `--deny NAME`: a violation when `name` resolves anywhere reachable
/// from the workspace members over `allowed` edges.
pub fn deny_paths(graph: &Graph, name: &str, allowed: &HashSet<EdgeKind>) -> Option<Violation> {
    let members: Vec<&str> = graph.members.iter().map(String::as_str).collect();
    graph.path_to(&members, name, allowed).map(|path| Violation {
        check: "deny".to_string(),
        subject: name.to_string(),
        evidence: path.join(" -> "),
    })
}

/// Evaluate `--forbid 'A=>B'`: a violation when any package named `from`
/// reaches a package named `to` over `allowed` edges. `Err` when `from` is
/// not in the graph at all (a defective assertion, not a clean pass).
pub fn forbid_path(
    graph: &Graph,
    from: &str,
    to: &str,
    allowed: &HashSet<EdgeKind>,
) -> Result<Option<Violation>, String> {
    let starts = graph.ids_named(from);
    if starts.is_empty() {
        return Err(format!("--forbid: no package named '{from}' in the graph"));
    }
    Ok(graph.path_to(&starts, to, allowed).map(|path| Violation {
        check: "forbid".to_string(),
        subject: format!("{from}=>{to}"),
        evidence: path.join(" -> "),
    }))
}

/// Evaluate `--acyclic`: one [`Violation`] per dependency cycle — a
/// strongly-connected component of two or more crates, or a self-loop — over
/// `allowed` edges. Evidence is a concrete cycle path; the subject lists the
/// crate names in the cycle. Output is sorted for determinism. When
/// `members_only` is set, only cycles among workspace members are reported
/// (the actionable form: crate cycles among third-party deps are almost always
/// dev-dependency noise you cannot fix).
pub fn cycles(graph: &Graph, allowed: &HashSet<EdgeKind>, members_only: bool) -> Vec<Violation> {
    let member_set: HashSet<&str> = graph.members.iter().map(String::as_str).collect();
    let restrict = members_only.then_some(&member_set);
    let mut out = Vec::new();
    for scc in graph.strongly_connected(allowed, restrict) {
        let self_loop = scc.len() == 1
            && graph
                .edges
                .get(&scc[0])
                .map(Vec::as_slice)
                .unwrap_or(&[])
                .iter()
                .any(|(dep, kinds)| dep == &scc[0] && kinds.iter().any(|k| allowed.contains(k)));
        if scc.len() < 2 && !self_loop {
            continue;
        }
        let set: HashSet<&str> = scc.iter().map(String::as_str).collect();
        let start = scc.iter().min().map(String::as_str).expect("non-empty scc");
        let evidence = graph.shortest_cycle_through(start, &set, allowed).join(" -> ");
        let mut names: Vec<&str> = scc
            .iter()
            .filter_map(|id| graph.packages.get(id).map(|p| p.name.as_str()))
            .collect();
        names.sort_unstable();
        names.dedup();
        out.push(Violation {
            check: "acyclic".to_string(),
            subject: names.join(", "),
            evidence,
        });
    }
    out.sort_by(|a, b| a.subject.cmp(&b.subject));
    out
}

/// A layer in a `--layers` stack: its label paired with the ids of the members
/// assigned to it, in input order.
pub type Layer = (String, Vec<String>);

/// Assign each workspace member to a layer by the `matches(index, name)`
/// predicate. Returns the layers (label + member ids, input order preserved)
/// and the names of members matching no layer. Errors when a member matches
/// more than one layer — an ambiguous spec, not a clean pass.
pub fn assign_layers(
    graph: &Graph,
    labels: &[String],
    matches: impl Fn(usize, &str) -> bool,
) -> Result<(Vec<Layer>, Vec<String>), String> {
    let mut layers: Vec<Layer> =
        labels.iter().map(|l| (l.clone(), Vec::new())).collect();
    let mut unassigned = Vec::new();
    for id in &graph.members {
        let name = graph.packages.get(id).map(|p| p.name.as_str()).unwrap_or("");
        let hit: Vec<usize> = (0..labels.len()).filter(|&i| matches(i, name)).collect();
        match hit.as_slice() {
            [] => unassigned.push(name.to_string()),
            [one] => layers[*one].1.push(id.clone()),
            many => {
                return Err(format!(
                    "crate '{name}' matches multiple layers ({})",
                    many.iter().map(|&m| labels[m].as_str()).collect::<Vec<_>>().join(", ")
                ));
            }
        }
    }
    // A layer that captures no member is almost always a typo (the silent
    // footgun: its intended constraints would vanish). Refuse it, like
    // `--forbid` refuses an absent source package.
    if let Some((label, _)) = layers.iter().find(|(_, ids)| ids.is_empty()) {
        return Err(format!("layer '{label}' matches nothing"));
    }
    Ok((layers, unassigned))
}

/// Evaluate `--layers`: `layers` are ordered **highest first** — a layer may
/// depend on layers listed after it, never before. One [`Violation`] per
/// offending **member** of a lower layer that reaches a higher layer over
/// `allowed` edges (so every violator is named, not just the first), each
/// carrying its dependency path.
pub fn layer_violations(
    graph: &Graph,
    layers: &[Layer],
    allowed: &HashSet<EdgeKind>,
) -> Vec<Violation> {
    let mut out = Vec::new();
    for lower in 0..layers.len() {
        for higher in 0..lower {
            let targets: HashSet<&str> = layers[higher].1.iter().map(String::as_str).collect();
            if targets.is_empty() {
                continue;
            }
            for start in &layers[lower].1 {
                if let Some(path) = graph.path_to_ids(&[start.as_str()], &targets, allowed) {
                    out.push(Violation {
                        check: "layers".to_string(),
                        subject: format!("{} => {}", layers[lower].0, layers[higher].0),
                        evidence: path.join(" -> "),
                    });
                }
            }
        }
    }
    out
}

// ----- The `deps` built-in check -------------------------------------------------

/// The assertion flags of a `deps` built-in check. No framing
/// (question / emit / json): the rule layer owns the verdict; this only asserts.
#[derive(Parser, Debug)]
#[command(no_binary_name = true, disable_help_flag = true)]
struct DepsCheck {
    #[arg(long, value_name = "NAME")]
    deny: Vec<String>,
    #[arg(long, value_name = "A=>B")]
    forbid: Vec<String>,
    #[arg(long)]
    duplicates: bool,
    #[arg(long)]
    acyclic: bool,
    #[arg(long)]
    members: bool,
    #[arg(long, value_name = "L0,L1,...", value_delimiter = ',')]
    layers: Vec<String>,
    #[arg(long)]
    layers_closed: bool,
    #[arg(long, value_enum, value_delimiter = ',')]
    edges: Vec<EdgeKind>,
}

/// One long-flagged argument, read from the clap grammar: its `--name`, value
/// `kind` (`"boolean"` / `"array"` / `"string"`, the `docs/explain` `type`
/// vocabulary), whether clap requires it, and its enum `values` (empty when the
/// value is free-form).
pub struct FlagSpec {
    pub name: String,
    pub kind: &'static str,
    pub required: bool,
    pub values: Vec<String>,
}

/// The introspected grammar of a tool or built-in check: every long flag's
/// spec, plus the names of all clap-required arguments — flags *and* positionals
/// (named by their field id, e.g. `path`/`probe`). The single source of truth
/// behind the published `docs/explain` schema (a test reconciles the two) and
/// the valid-flags hint on a bad argument.
pub struct Grammar {
    pub flags: Vec<FlagSpec>,
    pub required: Vec<String>,
}

/// Read a command's [`Grammar`]. Skips the auto help/version args and the
/// `--explain` documentation flag, none of which are tool inputs.
pub(crate) fn grammar(command: clap::Command) -> Grammar {
    let mut flags = Vec::new();
    let mut required = Vec::new();
    for arg in command.get_arguments() {
        let id = arg.get_id().as_str();
        if matches!(id, "help" | "version" | "explain") {
            continue;
        }
        // Positionals (path/probe/command/args) have no long flag; name them by
        // field id, matching the schema property key.
        let name = arg.get_long().map(String::from).unwrap_or_else(|| id.to_string());
        if arg.is_required_set() {
            required.push(name.clone());
        }
        if arg.get_long().is_some() {
            let kind = match arg.get_action() {
                clap::ArgAction::SetTrue | clap::ArgAction::SetFalse => "boolean",
                clap::ArgAction::Append => "array",
                _ => "string",
            };
            let values = arg.get_possible_values().iter().map(|v| v.get_name().to_string()).collect();
            flags.push(FlagSpec { name, kind, required: arg.is_required_set(), values });
        }
    }
    Grammar { flags, required }
}

/// The `deps` check's introspected grammar (see [`grammar`]).
pub fn check_grammar() -> Grammar {
    grammar(DepsCheck::command())
}

/// Run a `deps` built-in check over the crate graph rooted at `root` (one
/// hermetic `cargo metadata` invocation). Returns the probe outcome, a one-line
/// reason, and the violation report (one `check: subject: evidence` line each).
/// Argument, spec, and cargo errors are [`ProbeOutcome::Broken`] — a defective
/// probe, never a silent pass.
pub fn check(args: &[String], root: &Path, timeout: Option<Duration>) -> (ProbeOutcome, String, String) {
    let broken = |msg: String| (ProbeOutcome::Broken, msg, String::new());
    let cli = match DepsCheck::try_parse_from(args.iter().map(String::as_str)) {
        Ok(c) => c,
        Err(e) => {
            let valid = check_grammar().flags.iter().map(|s| format!("--{}", s.name)).collect::<Vec<_>>().join(" ");
            return broken(format!(
                "deps: {} (valid flags: {valid})",
                e.to_string().lines().next().unwrap_or("bad arguments")
            ));
        }
    };
    if cli.layers_closed && cli.layers.is_empty() {
        return broken("deps: --layers-closed requires --layers".to_string());
    }
    if cli.members && !cli.acyclic {
        return broken("deps: --members applies to --acyclic".to_string());
    }
    if cli.deny.is_empty() && cli.forbid.is_empty() && !cli.duplicates && !cli.acyclic && cli.layers.is_empty() {
        return broken("deps: nothing to assert (--deny/--forbid/--duplicates/--acyclic/--layers)".to_string());
    }
    let allowed: HashSet<EdgeKind> = if cli.edges.is_empty() {
        [EdgeKind::Normal, EdgeKind::Build, EdgeKind::Dev].into_iter().collect()
    } else {
        cli.edges.iter().copied().collect()
    };
    let forbids: Vec<(String, String)> = match cli
        .forbid
        .iter()
        .map(|spec| {
            spec.split_once("=>")
                .map(|(a, b)| (a.trim().to_string(), b.trim().to_string()))
                .filter(|(a, b)| !a.is_empty() && !b.is_empty())
                .ok_or_else(|| format!("deps: --forbid needs 'A=>B', got '{spec}'"))
        })
        .collect()
    {
        Ok(f) => f,
        Err(e) => return broken(e),
    };

    let mut command = Command::new("cargo");
    command
        .args(["metadata", "--format-version", "1", "--locked", "--offline"])
        .current_dir(root);
    let outcome = match supervise::run_captured(command, None, timeout) {
        Ok(o) => o,
        Err(e) => return broken(format!("deps: cargo metadata: {e}")),
    };
    if outcome.timed_out {
        return broken("deps: cargo metadata timed out".to_string());
    }
    if !outcome.status.is_some_and(|s| s.success()) {
        return broken(format!(
            "deps: cargo metadata failed: {}",
            outcome.stderr.lines().last().unwrap_or("(no output)")
        ));
    }
    let graph = match parse_metadata(&outcome.stdout) {
        Ok(g) => g,
        Err(e) => return broken(format!("deps: {e}")),
    };

    let mut violations: Vec<Violation> = Vec::new();
    for name in &cli.deny {
        violations.extend(deny_paths(&graph, name, &allowed));
    }
    for (from, to) in &forbids {
        match forbid_path(&graph, from, to, &allowed) {
            Ok(v) => violations.extend(v),
            Err(e) => return broken(format!("deps: {e}")),
        }
    }
    if cli.duplicates {
        for (name, versions) in graph.duplicates() {
            violations.push(Violation {
                check: "duplicates".to_string(),
                subject: name,
                evidence: versions.join(", "),
            });
        }
    }
    if cli.acyclic {
        violations.extend(cycles(&graph, &allowed, cli.members));
    }
    if !cli.layers.is_empty() {
        let compiled = match cli.layers.iter().map(|p| pattern::compile_anchored(p)).collect::<Result<Vec<_>, _>>() {
            Ok(c) => c,
            Err(e) => return broken(format!("deps: --layers invalid pattern: {e}")),
        };
        let (layers, unassigned) =
            match assign_layers(&graph, &cli.layers, |i, n| compiled[i].is_match(n)) {
                Ok(r) => r,
                Err(e) => return broken(format!("deps: --layers: {e}")),
            };
        violations.extend(layer_violations(&graph, &layers, &allowed));
        if cli.layers_closed {
            violations.extend(unassigned.into_iter().map(|name| Violation {
                check: "layers-closed".to_string(),
                subject: name,
                evidence: "matches no layer".to_string(),
            }));
        }
    }

    report_outcome("deps", violations)
}

/// The `(outcome, reason, report)` triple shared by both built-in checks: a
/// newline-joined `check: subject: evidence` report, `Holds` when empty.
pub(crate) fn report_outcome(kind: &str, violations: Vec<Violation>) -> (ProbeOutcome, String, String) {
    let report = violations
        .iter()
        .map(|v| format!("{}: {}: {}", v.check, v.subject, v.evidence))
        .collect::<Vec<_>>()
        .join("\n");
    if violations.is_empty() {
        (ProbeOutcome::Holds, format!("{kind}: all assertions hold"), report)
    } else {
        (ProbeOutcome::Violated, format!("{kind}: {} violation(s)", violations.len()), report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny metadata document: member `app` -> `lib` -> `leaf v1`, plus a
    /// dev-only edge `app -(dev)-> leaf v2` (a duplicate version).
    fn sample() -> Graph {
        let json = r#"{
          "packages": [
            {"id": "app-id", "name": "app", "version": "0.1.0"},
            {"id": "lib-id", "name": "lib", "version": "0.1.0"},
            {"id": "leaf1-id", "name": "leaf", "version": "1.0.0"},
            {"id": "leaf2-id", "name": "leaf", "version": "2.0.0"}
          ],
          "workspace_members": ["app-id"],
          "resolve": {"nodes": [
            {"id": "app-id", "deps": [
              {"name": "lib", "pkg": "lib-id", "dep_kinds": [{"kind": null}]},
              {"name": "leaf", "pkg": "leaf2-id", "dep_kinds": [{"kind": "dev"}]}
            ]},
            {"id": "lib-id", "deps": [
              {"name": "leaf", "pkg": "leaf1-id", "dep_kinds": [{"kind": null}]}
            ]},
            {"id": "leaf1-id", "deps": []},
            {"id": "leaf2-id", "deps": []}
          ]}
        }"#;
        parse_metadata(json).unwrap()
    }

    fn all_edges() -> HashSet<EdgeKind> {
        [EdgeKind::Normal, EdgeKind::Build, EdgeKind::Dev]
            .into_iter()
            .collect()
    }

    #[test]
    fn deny_reports_an_evidence_path() {
        let g = sample();
        let v = deny_paths(&g, "leaf", &all_edges()).expect("leaf is reachable");
        assert_eq!(v.check, "deny");
        // BFS finds the shortest route: the direct dev edge to leaf v2.
        assert_eq!(v.evidence, "app v0.1.0 -> leaf v2.0.0");
        assert!(deny_paths(&g, "absent", &all_edges()).is_none());
    }

    #[test]
    fn edge_kind_filter_changes_reachability() {
        let g = sample();
        let normal: HashSet<EdgeKind> = [EdgeKind::Normal].into_iter().collect();
        // Over normal edges only, the route goes through lib.
        let v = deny_paths(&g, "leaf", &normal).unwrap();
        assert_eq!(v.evidence, "app v0.1.0 -> lib v0.1.0 -> leaf v1.0.0");
        // A dev-only target disappears when dev edges are excluded.
        let dev_only: HashSet<EdgeKind> = [EdgeKind::Dev].into_iter().collect();
        let v = deny_paths(&g, "leaf", &dev_only).unwrap();
        assert_eq!(v.evidence, "app v0.1.0 -> leaf v2.0.0");
    }

    #[test]
    fn forbid_requires_the_source_to_exist() {
        let g = sample();
        let v = forbid_path(&g, "lib", "leaf", &all_edges()).unwrap().unwrap();
        assert_eq!(v.subject, "lib=>leaf");
        assert_eq!(v.evidence, "lib v0.1.0 -> leaf v1.0.0");
        assert!(forbid_path(&g, "lib", "app", &all_edges()).unwrap().is_none());
        assert!(forbid_path(&g, "ghost", "leaf", &all_edges()).is_err());
    }

    #[test]
    fn duplicates_lists_versions() {
        let g = sample();
        let d = g.duplicates();
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].0, "leaf");
        assert_eq!(d[0].1, ["1.0.0", "2.0.0"]);
    }

    /// `a -> b -> c -> a` (a 3-cycle) plus an acyclic `c -> d`.
    fn cyclic() -> Graph {
        let json = r#"{
          "packages": [
            {"id": "a", "name": "a", "version": "1.0.0"},
            {"id": "b", "name": "b", "version": "1.0.0"},
            {"id": "c", "name": "c", "version": "1.0.0"},
            {"id": "d", "name": "d", "version": "1.0.0"}
          ],
          "workspace_members": ["a"],
          "resolve": {"nodes": [
            {"id": "a", "deps": [{"name": "b", "pkg": "b", "dep_kinds": [{"kind": null}]}]},
            {"id": "b", "deps": [{"name": "c", "pkg": "c", "dep_kinds": [{"kind": null}]}]},
            {"id": "c", "deps": [
              {"name": "a", "pkg": "a", "dep_kinds": [{"kind": null}]},
              {"name": "d", "pkg": "d", "dep_kinds": [{"kind": null}]}
            ]},
            {"id": "d", "deps": []}
          ]}
        }"#;
        parse_metadata(json).unwrap()
    }

    #[test]
    fn cycles_report_a_concrete_loop() {
        let g = cyclic();
        let v = cycles(&g, &all_edges(), false);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].check, "acyclic");
        assert_eq!(v[0].subject, "a, b, c");
        assert_eq!(v[0].evidence, "a v1.0.0 -> b v1.0.0 -> c v1.0.0 -> a v1.0.0");
    }

    #[test]
    fn acyclic_graph_has_no_cycles() {
        // app -> lib -> leaf, plus a dev edge to a second leaf: no cycle.
        assert!(cycles(&sample(), &all_edges(), false).is_empty());
    }

    /// `x -> y` (normal) and `y -> x` (dev): a cycle only when dev edges count.
    fn cyclic_via_dev() -> Graph {
        let json = r#"{
          "packages": [
            {"id": "x", "name": "x", "version": "1.0.0"},
            {"id": "y", "name": "y", "version": "1.0.0"}
          ],
          "workspace_members": ["x"],
          "resolve": {"nodes": [
            {"id": "x", "deps": [{"name": "y", "pkg": "y", "dep_kinds": [{"kind": null}]}]},
            {"id": "y", "deps": [{"name": "x", "pkg": "x", "dep_kinds": [{"kind": "dev"}]}]}
          ]}
        }"#;
        parse_metadata(json).unwrap()
    }

    #[test]
    fn cycles_respect_edge_kinds() {
        let g = cyclic_via_dev();
        let normal: HashSet<EdgeKind> = [EdgeKind::Normal].into_iter().collect();
        assert!(cycles(&g, &normal, false).is_empty());
        assert_eq!(cycles(&g, &all_edges(), false).len(), 1);
    }

    /// Three workspace members; `svc -> api` is an upward (illegal) edge.
    fn layered() -> Graph {
        let json = r#"{
          "packages": [
            {"id": "api", "name": "api", "version": "1.0.0"},
            {"id": "svc", "name": "svc", "version": "1.0.0"},
            {"id": "db", "name": "db", "version": "1.0.0"}
          ],
          "workspace_members": ["api", "svc", "db"],
          "resolve": {"nodes": [
            {"id": "api", "deps": []},
            {"id": "svc", "deps": [{"name": "api", "pkg": "api", "dep_kinds": [{"kind": null}]}]},
            {"id": "db", "deps": []}
          ]}
        }"#;
        parse_metadata(json).unwrap()
    }

    #[test]
    fn layers_flag_a_lower_layer_reaching_a_higher_one() {
        let g = layered();
        let labels = vec!["api".to_string(), "svc".to_string(), "db".to_string()];
        // Exact-name membership for the test.
        let (layers, unassigned) =
            assign_layers(&g, &labels, |i, name| labels[i] == name).unwrap();
        assert!(unassigned.is_empty());
        let v = layer_violations(&g, &layers, &all_edges());
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].check, "layers");
        assert_eq!(v[0].subject, "svc => api");
        assert_eq!(v[0].evidence, "svc v1.0.0 -> api v1.0.0");
    }

    #[test]
    fn layers_allow_top_down_dependencies() {
        // Same crates, but ordered svc above api: svc -> api is now legal.
        let g = layered();
        let labels = vec!["svc".to_string(), "api".to_string(), "db".to_string()];
        let (layers, _) = assign_layers(&g, &labels, |i, name| labels[i] == name).unwrap();
        assert!(layer_violations(&g, &layers, &all_edges()).is_empty());
    }

    #[test]
    fn assign_layers_reports_unassigned_and_ambiguous() {
        let g = layered();
        // db matches no layer -> unassigned (the --layers-closed signal).
        let labels = vec!["api".to_string(), "svc".to_string()];
        let (_, unassigned) = assign_layers(&g, &labels, |i, name| labels[i] == name).unwrap();
        assert_eq!(unassigned, ["db"]);
        // A predicate that matches `api` in two layers is an ambiguous spec.
        let two = vec!["api".to_string(), "api-again".to_string()];
        let err = assign_layers(&g, &two, |_, name| name == "api").unwrap_err();
        assert!(err.contains("multiple layers"), "got: {err}");
    }

    #[test]
    fn assign_layers_rejects_an_empty_layer() {
        let g = layered();
        // `ghostlayer` matches no member: a typo that would silently drop its rules.
        let labels = vec!["api".to_string(), "ghostlayer".to_string()];
        let err = assign_layers(&g, &labels, |i, name| labels[i] == name).unwrap_err();
        assert!(err.contains("matches nothing"), "got: {err}");
    }

    /// A member cycle (`a <-> b` via a dev back-edge) and a separate external
    /// cycle (`x -> y -> x`) reachable from a member.
    fn mixed_cycles() -> Graph {
        let json = r#"{
          "packages": [
            {"id": "a", "name": "a", "version": "1.0.0"},
            {"id": "b", "name": "b", "version": "1.0.0"},
            {"id": "x", "name": "x", "version": "1.0.0"},
            {"id": "y", "name": "y", "version": "1.0.0"}
          ],
          "workspace_members": ["a", "b"],
          "resolve": {"nodes": [
            {"id": "a", "deps": [
              {"name": "b", "pkg": "b", "dep_kinds": [{"kind": null}]},
              {"name": "x", "pkg": "x", "dep_kinds": [{"kind": null}]}
            ]},
            {"id": "b", "deps": [{"name": "a", "pkg": "a", "dep_kinds": [{"kind": "dev"}]}]},
            {"id": "x", "deps": [{"name": "y", "pkg": "y", "dep_kinds": [{"kind": null}]}]},
            {"id": "y", "deps": [{"name": "x", "pkg": "x", "dep_kinds": [{"kind": null}]}]}
          ]}
        }"#;
        parse_metadata(json).unwrap()
    }

    #[test]
    fn cycles_members_only_scopes_to_workspace() {
        let g = mixed_cycles();
        // Whole graph: both the member cycle and the external one, sorted.
        let all = cycles(&g, &all_edges(), false);
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].subject, "a, b");
        assert_eq!(all[1].subject, "x, y");
        // Members only: just the actionable a<->b cycle.
        let members = cycles(&g, &all_edges(), true);
        assert_eq!(members.len(), 1);
        assert_eq!(members[0].subject, "a, b");
    }

    /// `api <- svc <- db` chain: db reaches api only transitively.
    fn layered_transitive() -> Graph {
        let json = r#"{
          "packages": [
            {"id": "api", "name": "api", "version": "1.0.0"},
            {"id": "svc", "name": "svc", "version": "1.0.0"},
            {"id": "db", "name": "db", "version": "1.0.0"}
          ],
          "workspace_members": ["api", "svc", "db"],
          "resolve": {"nodes": [
            {"id": "api", "deps": []},
            {"id": "svc", "deps": [{"name": "api", "pkg": "api", "dep_kinds": [{"kind": null}]}]},
            {"id": "db", "deps": [{"name": "svc", "pkg": "svc", "dep_kinds": [{"kind": null}]}]}
          ]}
        }"#;
        parse_metadata(json).unwrap()
    }

    #[test]
    fn layers_report_transitive_violations_per_member() {
        let g = layered_transitive();
        let labels = vec!["api".to_string(), "svc".to_string(), "db".to_string()];
        let (layers, _) = assign_layers(&g, &labels, |i, name| labels[i] == name).unwrap();
        let v = layer_violations(&g, &layers, &all_edges());
        let by: BTreeMap<&str, &str> =
            v.iter().map(|x| (x.subject.as_str(), x.evidence.as_str())).collect();
        // db reaches api only through svc — the path proves the transitive hop.
        assert_eq!(by["db => api"], "db v1.0.0 -> svc v1.0.0 -> api v1.0.0");
        assert_eq!(by["svc => api"], "svc v1.0.0 -> api v1.0.0");
        assert_eq!(by["db => svc"], "db v1.0.0 -> svc v1.0.0");
        assert_eq!(v.len(), 3);
    }
}
