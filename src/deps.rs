// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Jonathan Shook

//! `ct-deps`'s crate-graph queries.
//!
//! The resolved dependency graph comes from `cargo metadata --format-version
//! 1 --locked --offline` (hermetic by construction); this module parses that
//! JSON into a [`Graph`] and answers the crate-level invariant questions:
//! *is crate X anywhere in the tree* ([`deny_paths`]), *does workspace member
//! A reach crate B* ([`forbid_path`]), and *do any crates resolve at more
//! than one version* ([`duplicates`]). Every violation carries its evidence —
//! a dependency path or the duplicated version list — so a red answer is
//! never just a name.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

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
            Some(p) => format!("{} v{}", p.name, p.version),
            None => id.to_string(),
        }
    }

    /// BFS from `starts` to the first package named `target`, traversing only
    /// `allowed` edge kinds. Returns the evidence path as `name vX` labels.
    pub fn path_to(
        &self,
        starts: &[&str],
        target: &str,
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
            if self.packages.get(id).is_some_and(|p| p.name == target)
                && !starts.contains(&id)
            {
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
}
