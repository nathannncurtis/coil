//! Keep rules: the `[compile] keep` patterns (spec section 6). Three forms:
//!
//! - `pkg.*` keeps the package `pkg` and every module under it: compiled
//!   whole, registered in the module table, no dead-code elimination, no
//!   sealing;
//! - `pkg.mod.*.name` keeps the top-level member `name` of every module the
//!   segments before it match, `*` matching one segment;
//! - `pkg.mod:Name` keeps the top-level class or function `Name` of `pkg.mod`.
//!
//! A module rule names modules the build could not see otherwise - the
//! declared dependency of a variable-driven `import_module` - so expansion
//! adds them to the module set. Member rules mark members of modules that
//! ended up in the set.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::PatternError;

/// One keep rule.
#[derive(Clone, Debug)]
enum Rule {
    /// `pkg.*`: the prefix package and every module under it.
    Modules { prefix: Vec<Segment> },
    /// `pkg.mod.*.name` / `pkg.mod.name` / `pkg.mod:name`: one member of
    /// every module the segments match.
    Member { module: Vec<Segment>, name: String },
}

/// One dotted-name segment: a literal name or `*`, matching one segment.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Segment {
    Name(String),
    Star,
}

impl Segment {
    fn matches(&self, part: &str) -> bool {
        match self {
            Segment::Name(name) => name == part,
            Segment::Star => true,
        }
    }
}

/// The parsed `[compile] keep` rules.
#[derive(Clone, Debug, Default)]
pub struct Rules {
    rules: Vec<Rule>,
}

impl Rules {
    /// Parse the job's keep patterns.
    pub fn parse(patterns: &[String]) -> Result<Rules, PatternError> {
        let mut rules = Vec::new();
        for pattern in patterns {
            rules.push(parse_one(pattern)?);
        }
        Ok(Rules { rules })
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// Whether a module rule keeps `name`: the rule's prefix itself, or any
    /// module under it (spec section 6: `plugins.*` is "every module under
    /// plugins", the package included, so `import_module` finds it).
    pub fn keeps_module(&self, name: &str) -> bool {
        self.rules.iter().any(|rule| match rule {
            Rule::Modules { prefix } => module_matches_prefix(prefix, name),
            Rule::Member { .. } => false,
        })
    }

    /// The member names a rule keeps in `module`, in rule order.
    pub fn member_keeps(&self, module: &str) -> Vec<&str> {
        let mut names = Vec::new();
        for rule in &self.rules {
            if let Rule::Member {
                module: segments,
                name,
            } = rule
                && segments_match(segments, module)
            {
                names.push(name.as_str());
            }
        }
        names
    }

    /// Resolve the module rules against the source roots: every module a
    /// `pkg.*` rule names, mapped to the pattern that named it. The rule
    /// names modules nothing statically imports, so they are found by
    /// walking the source tree under the resolved prefix.
    pub fn expand_modules(&self, roots: &[PathBuf]) -> BTreeMap<String, String> {
        let mut found = BTreeMap::new();
        for rule in &self.rules {
            let Rule::Modules { prefix } = rule else {
                continue;
            };
            let name = prefix
                .iter()
                .filter_map(|segment| match segment {
                    Segment::Name(name) => Some(name.as_str()),
                    Segment::Star => None,
                })
                .collect::<Vec<_>>()
                .join(".");
            let pattern = format!("{name}.*");
            for module in resolve_tree(&name, roots) {
                found.insert(module, pattern.clone());
            }
        }
        found
    }
}

fn parse_one(pattern: &str) -> Result<Rule, PatternError> {
    let error = |message: &str| PatternError {
        pattern: pattern.to_owned(),
        message: message.to_owned(),
    };
    let (module_part, name) = match pattern.split_once(':') {
        Some((module, name)) => {
            let name = valid_member(name).ok_or_else(|| error("invalid name after ':'"))?;
            (module, Some(name))
        }
        None => (pattern, None),
    };
    let empty = if name.is_some() {
        "the module part before ':' is empty"
    } else {
        "the pattern is empty"
    };
    let segments = segments_of(module_part)
        .map_err(|e| error(&e))?
        .ok_or_else(|| error(empty))?;
    if let Some(name) = name {
        return Ok(Rule::Member {
            module: segments,
            name,
        });
    }
    match segments.last() {
        Some(Segment::Star) => {
            let prefix = &segments[..segments.len() - 1];
            if prefix.contains(&Segment::Star) {
                return Err(error("a '*' may only end the module part of a '*' rule"));
            }
            if prefix.is_empty() {
                return Err(error("a lone '*' keeps nothing; name the package first"));
            }
            Ok(Rule::Modules {
                prefix: prefix.to_vec(),
            })
        }
        Some(Segment::Name(name)) => {
            let module = segments[..segments.len() - 1].to_vec();
            if module.is_empty() {
                return Err(error("a member rule needs a module before the member name"));
            }
            Ok(Rule::Member {
                module,
                name: name.clone(),
            })
        }
        None => Err(error(empty)),
    }
}

/// Split a dotted module part into segments; `None` for an empty part.
fn segments_of(part: &str) -> Result<Option<Vec<Segment>>, String> {
    if part.is_empty() {
        return Ok(None);
    }
    let mut segments = Vec::new();
    for piece in part.split('.') {
        if piece == "*" {
            segments.push(Segment::Star);
        } else if let Some(name) = valid_member(piece) {
            segments.push(Segment::Name(name));
        } else {
            return Err(format!("invalid segment '{piece}'"));
        }
    }
    Ok(Some(segments))
}

/// A plain identifier segment: letters, digits and underscores, not starting
/// with a digit.
fn valid_member(text: &str) -> Option<String> {
    let mut chars = text.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return None,
    }
    if chars.all(|c| c.is_ascii_alphanumeric() || c == '_') {
        Some(text.to_owned())
    } else {
        None
    }
}

/// Whether `name` is the prefix package itself or lies under it.
fn module_matches_prefix(prefix: &[Segment], name: &str) -> bool {
    let parts: Vec<&str> = name.split('.').collect();
    if parts.len() < prefix.len() {
        return false;
    }
    prefix
        .iter()
        .zip(&parts)
        .all(|(segment, part)| segment.matches(part))
}

/// Whether the segments match a whole dotted module name, one segment each.
fn segments_match(segments: &[Segment], name: &str) -> bool {
    let parts: Vec<&str> = name.split('.').collect();
    parts.len() == segments.len()
        && segments
            .iter()
            .zip(&parts)
            .all(|(segment, part)| segment.matches(part))
}

/// Every module `name` resolves to under the roots: itself, plus its
/// submodules when it is a package. Modules of an unresolvable name match
/// nothing.
fn resolve_tree(name: &str, roots: &[PathBuf]) -> Vec<String> {
    let parts: Vec<&str> = name.split('.').collect();
    for root in roots {
        let mut dir = root.to_path_buf();
        for part in &parts {
            dir.push(part);
        }
        if dir.join("__init__.py").is_file() {
            let mut modules = vec![name.to_owned()];
            modules.extend(submodules_under(&dir, name));
            return modules;
        }
        if dir.with_extension("py").is_file() {
            return vec![name.to_owned()];
        }
    }
    Vec::new()
}

/// Every module under the package directory `dir`, named `prefix.part`.
fn submodules_under(dir: &Path, prefix: &str) -> Vec<String> {
    let mut modules = Vec::new();
    let mut stack = vec![(dir.to_path_buf(), prefix.to_owned())];
    while let Some((dir, prefix)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let name = format!("{prefix}.{stem}");
            if file_type.is_file() {
                if path.extension().and_then(|e| e.to_str()) == Some("py") && stem != "__init__" {
                    modules.push(name);
                }
            } else if file_type.is_dir() && path.join("__init__.py").is_file() {
                modules.push(name.clone());
                stack.push((path, name));
            }
        }
    }
    modules.sort();
    modules
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(patterns: &[&str]) -> Rules {
        Rules::parse(&patterns.iter().map(|p| p.to_string()).collect::<Vec<_>>()).unwrap()
    }

    #[test]
    fn module_rules_keep_the_package_and_its_tree() {
        let r = rules(&["plugins.*"]);
        assert!(r.keeps_module("plugins"));
        assert!(r.keeps_module("plugins.alpha"));
        assert!(r.keeps_module("plugins.sub.deep"));
        assert!(!r.keeps_module("plugins_and_more"));
        assert!(!r.keeps_module("app"));
        assert!(r.member_keeps("plugins").is_empty());
    }

    #[test]
    fn member_rules_match_whole_names_and_stars() {
        let r = rules(&[
            "myapp.models.*.to_dict",
            "myapp.registry:Handler",
            "app.helper:before",
        ]);
        assert_eq!(r.member_keeps("myapp.models.user"), ["to_dict"]);
        assert_eq!(r.member_keeps("myapp.models"), Vec::<&str>::new());
        assert!(!r.keeps_module("myapp.models.user"));
        assert_eq!(r.member_keeps("myapp.registry"), ["Handler"]);
        assert_eq!(r.member_keeps("app.helper"), ["before"]);
    }

    #[test]
    fn rejects_malformed_patterns() {
        for (pattern, message) in [
            ("", "is empty"),
            (".", "invalid segment"),
            ("*", "keeps nothing"),
            ("..a", "invalid segment"),
            ("pkg.*.sub.*", "may only end"),
            ("mod", "needs a module"),
            (":Name", "before ':' is empty"),
            ("pkg.mod:", "invalid name after ':'"),
            ("pkg.mod:Na me", "invalid name after ':'"),
        ] {
            let error = Rules::parse(&[pattern.to_owned()]).unwrap_err();
            assert_eq!(error.pattern, pattern);
            assert!(error.message.contains(message), "{pattern}: {error}");
        }
    }

    #[test]
    fn expands_module_rules_over_a_source_tree() {
        let dir = std::env::temp_dir().join(format!("coilc-keep-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for file in [
            "plugins/__init__.py",
            "plugins/alpha.py",
            "plugins/sub/__init__.py",
            "plugins/sub/beta.py",
            "other/__init__.py",
            "solo.py",
        ] {
            let path = dir.join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, "").unwrap();
        }
        let roots = vec![dir.clone()];
        let r = rules(&["plugins.*", "gone.*", "solo.*", "other:Table"]);
        let expanded = r.expand_modules(&roots);
        assert_eq!(
            expanded.keys().cloned().collect::<Vec<_>>(),
            [
                "plugins",
                "plugins.alpha",
                "plugins.sub",
                "plugins.sub.beta",
                "solo"
            ]
        );
        assert!(expanded.values().all(|p| p.ends_with(".*")));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
