//! Capability-scoped filesystem access for agent-callable import tools.
//!
//! File paths chosen by the agent loop are untrusted input: the model —
//! or a prompt-injected one — picks them. A naive tool that opened any
//! path it was handed is an arbitrary-file-read primitive (it could
//! reach `/etc`, signing seeds, or the off-limits OpenClaw data the box
//! cohabits with).
//!
//! Rather than blocklist `..` (which loses to symlinks and encodings),
//! we *confine by construction*: the operator designates one import
//! directory, the agent may only name files relative to it, and the
//! canonicalized result must remain a descendant of that root. `..`
//! traversal and symlink escapes are both rejected because we compare
//! real, link-resolved paths — not the string the agent supplied.
//!
//! This is least-privilege for the discovery layer: the agent operates
//! inside a sandbox it cannot escape, regardless of what it is coaxed
//! into asking for. The operator-run CLI keeps the unconfined
//! [`crate::CsvSeed::read_path`] primitive — its trust boundary is the
//! operator's own shell, not the model.

use salesman_core::{Error, Result};
use std::path::{Component, Path, PathBuf};

/// Environment variable naming the operator-controlled import directory.
pub const IMPORT_DIR_ENV: &str = "SALESMAN_IMPORT_DIR";

/// The operator-designated directory that agent-supplied import paths
/// are confined to.
///
/// Construct with [`ImportRoot::from_env`] (production) or
/// [`ImportRoot::new`] (tests / explicit wiring). All paths handed to
/// [`ImportRoot::resolve`] are guaranteed to land inside this root.
#[derive(Debug, Clone)]
pub struct ImportRoot {
    /// Canonical (symlink-resolved) absolute path to the import dir.
    root: PathBuf,
}

impl ImportRoot {
    /// Build an [`ImportRoot`] from the [`IMPORT_DIR_ENV`] variable.
    ///
    /// Fail-closed: if the variable is unset, agent-driven imports are
    /// refused with a clear message. The operator opts in by creating a
    /// directory and pointing the variable at it — there is no implicit
    /// fallback to the working directory or `/`.
    pub fn from_env() -> Result<Self> {
        Self::from_var(std::env::var(IMPORT_DIR_ENV).ok())
    }

    /// Build from an already-resolved optional directory value.
    ///
    /// Split out of [`ImportRoot::from_env`] so the fail-closed branch
    /// is testable without mutating the process environment.
    fn from_var(value: Option<String>) -> Result<Self> {
        let raw = value.ok_or_else(|| {
            Error::Validation(format!(
                "agent CSV import is disabled: set {IMPORT_DIR_ENV} to an \
                 operator-controlled directory to enable it"
            ))
        })?;
        // `raw` is operator-controlled (an env var), and `new` below
        // canonicalizes + dir-checks it. nosemgrep
        Self::new(PathBuf::from(raw)) // nosemgrep
    }

    /// Build an [`ImportRoot`] rooted at `dir`.
    ///
    /// The directory must already exist; it is canonicalized so later
    /// containment checks compare real, symlink-resolved paths.
    pub fn new(dir: PathBuf) -> Result<Self> {
        let root = std::fs::canonicalize(&dir)
            .map_err(|e| Error::Validation(format!("import dir {}: {e}", dir.display())))?;
        if !root.is_dir() {
            return Err(Error::Validation(format!(
                "import dir {} is not a directory",
                root.display()
            )));
        }
        Ok(Self { root })
    }

    /// The canonical root directory this jail confines to.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Resolve an agent-supplied `name` to a real path *inside* this
    /// root, or return an error if it would escape.
    ///
    /// `name` must be relative (absolute paths are rejected outright)
    /// and must not contain `..`. The joined path is canonicalized and
    /// verified to remain a descendant of the root — defeating both
    /// `..` traversal and symlink escapes, since the comparison is on
    /// the link-resolved path rather than the literal input.
    pub fn resolve(&self, name: &str) -> Result<PathBuf> {
        // `name` is untrusted (agent-chosen). This function is exactly
        // the sanitizer: the checks below (absolute, `..`, canonicalize,
        // starts_with root) confine it before any read happens.
        let candidate = Path::new(name); // nosemgrep
        if candidate.is_absolute() {
            return Err(Error::Validation(format!(
                "import path `{name}` must be relative to {IMPORT_DIR_ENV}"
            )));
        }
        // Reject traversal components up front for a clear error; the
        // canonicalize-and-contain check below is the authoritative one.
        if candidate
            .components()
            .any(|c| matches!(c, Component::ParentDir))
        {
            return Err(Error::Validation(format!(
                "import path `{name}` must not contain `..`"
            )));
        }
        // This join feeds the canonicalize + starts_with(root) check
        // immediately below, which IS the path-traversal mitigation.
        let joined = self.root.join(candidate); // nosemgrep
        let real = std::fs::canonicalize(&joined)
            .map_err(|e| Error::Validation(format!("import path `{name}`: {e}")))?;
        if !real.starts_with(&self.root) {
            return Err(Error::Validation(format!(
                "import path `{name}` resolves outside {IMPORT_DIR_ENV}"
            )));
        }
        Ok(real)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// A unique temp directory for an isolated jail per test.
    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "salesman-import-{tag}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn resolves_a_file_inside_the_root() {
        let dir = temp_root("inside");
        let f = dir.join("companies.csv");
        std::fs::File::create(&f)
            .unwrap()
            .write_all(b"display_name\nAcme\n")
            .unwrap();

        let jail = ImportRoot::new(dir.clone()).unwrap();
        let resolved = jail.resolve("companies.csv").unwrap();
        assert!(resolved.starts_with(jail.root()));
        assert!(resolved.ends_with("companies.csv"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn rejects_absolute_paths() {
        let dir = temp_root("abs");
        let jail = ImportRoot::new(dir.clone()).unwrap();
        assert!(jail.resolve("/etc/passwd").is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn rejects_parent_dir_traversal() {
        let dir = temp_root("dotdot");
        let jail = ImportRoot::new(dir.clone()).unwrap();
        assert!(jail.resolve("../../etc/passwd").is_err());
        assert!(jail.resolve("sub/../../escape").is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_escape() {
        let dir = temp_root("symlink");
        // A symlink inside the root that points outside it.
        let link = dir.join("escape.csv");
        std::os::unix::fs::symlink("/etc/hostname", &link).unwrap();

        let jail = ImportRoot::new(dir.clone()).unwrap();
        // The link exists and canonicalizes, but to a path outside the
        // root — so containment must reject it.
        assert!(jail.resolve("escape.csv").is_err());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn from_var_is_fail_closed_when_unset() {
        // No process-env mutation needed: the disabled branch is driven
        // by the resolved value, so we exercise it directly.
        let err = ImportRoot::from_var(None).unwrap_err();
        assert!(format!("{err}").contains("disabled"));
    }
}
