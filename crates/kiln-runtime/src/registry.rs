//! The set of runtimes this build of Kiln knows about.

use std::sync::Arc;

use crate::provider::RuntimeProvider;
use crate::providers::{DenoProvider, GoProvider, NodeProvider, PythonProvider};

/// How different two names may be before Kiln stops offering a correction.
///
/// Scaled to the length of the runtime being compared against.
///
/// A fixed budget of two edits is fine for `python`. For a two-letter name like
/// `go` it matches almost anything — `io` and `ai` are one edit away, and the
/// empty string is two — so short names get a budget of zero and are matched by
/// shared prefix only.
///
/// The cost is that a one-character typo of `go` gets no suggestion. That is the
/// right trade: in two characters there is no way to tell a typo from a
/// different word, and a confident wrong guess sends someone down the wrong
/// path more expensively than saying nothing does.
fn suggestion_budget(candidate: &str) -> usize {
    if candidate.len() <= 3 { 0 } else { 2 }
}

/// The runtime providers available to this process.
#[derive(Clone)]
pub struct Registry {
    providers: Vec<Arc<dyn RuntimeProvider>>,
}

impl Registry {
    /// The providers compiled into Kiln.
    ///
    /// This list is the entire cost of adding a runtime. A future plugin system
    /// would extend the same vector at startup; nothing downstream cares where a
    /// provider came from.
    pub fn builtin() -> Self {
        Registry {
            providers: vec![
                Arc::new(NodeProvider),
                Arc::new(PythonProvider),
                Arc::new(GoProvider),
                Arc::new(DenoProvider),
            ],
        }
    }

    /// Build a registry from an explicit provider list. Used by tests.
    pub fn from_providers(providers: Vec<Arc<dyn RuntimeProvider>>) -> Self {
        Registry { providers }
    }

    /// Look a provider up by the identifier used in `kiln.toml`.
    pub fn get(&self, id: &str) -> Option<&Arc<dyn RuntimeProvider>> {
        self.providers.iter().find(|p| p.id() == id)
    }

    /// Whether Kiln knows this runtime.
    pub fn contains(&self, id: &str) -> bool {
        self.get(id).is_some()
    }

    /// Every provider, in a stable order.
    pub fn providers(&self) -> impl Iterator<Item = &Arc<dyn RuntimeProvider>> {
        self.providers.iter()
    }

    /// Every known identifier, sorted.
    pub fn ids(&self) -> Vec<&'static str> {
        let mut ids: Vec<&'static str> = self.providers.iter().map(|p| p.id()).collect();
        ids.sort_unstable();
        ids
    }

    /// The error to report when something names a runtime Kiln does not have.
    ///
    /// Lives here so that every caller — the resolver, `kiln init`, `kiln
    /// doctor` — words it the same way and offers the same correction.
    pub fn unknown(&self, name: &str) -> kiln_core::Error {
        let error = kiln_core::Error::not_found(format!("Kiln does not know the runtime `{name}`"))
            .because("no provider in this build of Kiln can install it")
            .expected(format!("one of: {}", self.ids().join(", ")));

        match self.suggest(name) {
            Some(suggestion) => error.hint(format!("did you mean `{suggestion}`?")),
            None => error.hint("adding a runtime means adding a provider; see CONTRIBUTING.md"),
        }
    }

    /// The known runtime an unrecognised name most likely meant.
    ///
    /// Returns `None` rather than the least-bad match when nothing is close:
    /// suggesting `node` to someone who typed `postgres` is worse than saying
    /// nothing, because it implies they made a typo rather than asked for
    /// something Kiln does not have.
    pub fn suggest(&self, unknown: &str) -> Option<&'static str> {
        let needle = unknown.to_ascii_lowercase();
        if needle.is_empty() {
            return None;
        }

        self.providers
            .iter()
            .filter_map(|provider| {
                let id = provider.id();

                // A shared prefix catches what edit distance cannot: `golang`
                // is four edits from `go`, and `nodejs` two from `node`, but
                // both are obviously the same word with something on the end.
                let related = needle.starts_with(id) || id.starts_with(&needle);
                let distance = levenshtein(&needle, id);

                if related {
                    Some((0, distance, id))
                } else if distance <= suggestion_budget(id) {
                    Some((1, distance, id))
                } else {
                    None
                }
            })
            // Prefix matches first, then the closest, then alphabetically so
            // the answer never depends on registration order.
            .min()
            .map(|(_, _, id)| id)
    }
}

impl Default for Registry {
    fn default() -> Self {
        Registry::builtin()
    }
}

impl std::fmt::Debug for Registry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Registry")
            .field("ids", &self.ids())
            .finish()
    }
}

/// Edit distance between two strings, using a single row of the usual matrix.
fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }

    let mut previous: Vec<usize> = (0..=b.len()).collect();
    let mut current = vec![0usize; b.len() + 1];

    for (i, &ca) in a.iter().enumerate() {
        current[0] = i + 1;
        for (j, &cb) in b.iter().enumerate() {
            let substitution = previous[j] + usize::from(ca != cb);
            let insertion = current[j] + 1;
            let deletion = previous[j + 1] + 1;
            current[j + 1] = substitution.min(insertion).min(deletion);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use kiln_core::Platform;

    #[test]
    fn the_builtin_registry_lists_every_runtime_in_sorted_order() {
        let registry = Registry::builtin();

        // Sorted, not registration order: `ids()` reaches error messages and
        // `--json`, and a list that reordered when a provider was added would
        // make every one of those outputs churn.
        assert_eq!(registry.ids(), ["deno", "go", "node", "python"]);
        for id in ["node", "python", "go", "deno"] {
            assert!(registry.contains(id), "{id} should be registered");
        }
        assert!(!registry.contains("ruby"));
    }

    #[test]
    fn lookup_returns_the_matching_provider() {
        let registry = Registry::builtin();
        assert_eq!(registry.get("node").unwrap().display_name(), "Node.js");
        assert_eq!(registry.get("python").unwrap().display_name(), "Python");
        assert!(registry.get("ruby").is_none());
    }

    #[test]
    fn every_provider_is_internally_consistent() {
        let platform = Platform::detect().expect("host platform");
        for provider in Registry::builtin().providers() {
            let id = provider.id();
            assert!(!id.is_empty(), "provider id must not be empty");
            assert_eq!(id, id.to_ascii_lowercase(), "`{id}` must be lowercase");
            assert!(!provider.display_name().is_empty(), "{id}");
            assert!(provider.homepage().starts_with("https://"), "{id}");

            // The default requirement is built from a constant; this is the test
            // the `expect` in each provider refers to.
            let requirement = provider.default_requirement();
            assert!(
                requirement.is_floating(),
                "`{id}` should propose a pin, not an exact version"
            );
            assert!(
                requirement.alias().is_none(),
                "`{id}` must not default to a floating alias"
            );

            assert!(
                provider.supports(&platform),
                "`{id}` should support the test host"
            );
        }
    }

    #[test]
    fn suggests_a_correction_for_near_misses() {
        let registry = Registry::builtin();
        assert_eq!(registry.suggest("nodejs"), Some("node"));
        assert_eq!(registry.suggest("Node"), Some("node"));
        assert_eq!(registry.suggest("pyton"), Some("python"));
        assert_eq!(registry.suggest("pythn"), Some("python"));
    }

    #[test]
    fn suggests_a_correction_for_the_longer_name_of_a_runtime() {
        // Edit distance alone never finds these: `golang` is four edits from
        // `go`. They are the single most likely thing for someone to type.
        let registry = Registry::builtin();
        assert_eq!(registry.suggest("golang"), Some("go"));
        assert_eq!(registry.suggest("python3"), Some("python"));
        assert_eq!(registry.suggest("py"), Some("python"));
    }

    #[test]
    fn stays_quiet_when_nothing_is_close() {
        let registry = Registry::builtin();
        assert_eq!(registry.suggest("postgres"), None);
        assert_eq!(registry.suggest("rust"), None);
        assert_eq!(registry.suggest(""), None);
    }

    #[test]
    fn a_short_runtime_name_does_not_match_everything() {
        // `go` is two characters, so a two-edit budget would match `io`, `ai`
        // and the empty string. Guessing `go` at someone who asked for Rust is
        // worse than admitting Kiln does not have it.
        let registry = Registry::builtin();
        for unrelated in ["io", "ai", "cc", "", "zig", "ada", "gp"] {
            assert_eq!(
                registry.suggest(unrelated),
                None,
                "`{unrelated}` should get no suggestion"
            );
        }
        // A shared prefix is the only signal short enough to trust.
        assert_eq!(registry.suggest("golang"), Some("go"));
    }

    #[test]
    fn the_suggestion_budget_scales_with_the_name() {
        assert_eq!(suggestion_budget("go"), 0);
        assert_eq!(suggestion_budget("node"), 2);
        assert_eq!(suggestion_budget("python"), 2);
    }

    #[test]
    fn edit_distance_is_symmetric_and_correct() {
        assert_eq!(levenshtein("", ""), 0);
        assert_eq!(levenshtein("node", "node"), 0);
        assert_eq!(levenshtein("node", "nodejs"), 2);
        assert_eq!(levenshtein("kitten", "sitting"), 3);
        assert_eq!(levenshtein("", "node"), 4);
        assert_eq!(levenshtein("node", ""), 4);
        assert_eq!(levenshtein("abc", "cba"), levenshtein("cba", "abc"));
    }
}
