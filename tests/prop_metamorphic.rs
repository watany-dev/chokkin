//! Metamorphic properties of the whole analysis on generated projects
//! (#1104): changes that cannot matter to a finding do not move it.
//!
//! - A file nothing imports adds only its own CHK001 (and what lives in it):
//!   an unreachable file's imports use nothing.
//! - Renaming a module everywhere moves its findings to the new name and
//!   changes no other.
//! - Neither depends on how many workers the run splits its files over: the
//!   generated projects straddle `MIN_ITEMS_PER_WORKER` (32 files), where a
//!   run goes from one worker to several, and the file added in the first
//!   property can push a project across it.

#![expect(
    clippy::expect_used,
    reason = "helpers outside `#[test]` fns abort the test on a failed setup step"
)]

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::fs;
use std::path::Path;
use std::process::Command;

use proptest::prelude::*;
use tempfile::TempDir;

/// Declared distributions the modules may import; no name is an affixed form
/// of another.
const POOL: [&str; 3] = ["zz-alpha", "zz-bravo", "zz-charlie"];

#[derive(Debug, Clone)]
struct Module {
    /// Imported from `main`.
    from_main: bool,
    /// Raw picks of earlier modules whose `used_*` this one calls.
    imports: Vec<usize>,
    /// Indices into `POOL` this module imports.
    deps: Vec<usize>,
}

#[derive(Debug, Clone)]
struct Project {
    modules: Vec<Module>,
}

fn module_strategy() -> impl Strategy<Value = Module> {
    (
        prop::bool::weighted(0.3),
        prop::collection::vec(any::<usize>(), 0..3),
        prop::collection::vec(0..POOL.len(), 0..2),
    )
        .prop_map(|(from_main, imports, deps)| Module {
            from_main,
            imports,
            deps,
        })
}

/// With `__init__.py` and `main.py`, 28 to 37 or 60 to 69 Python files.
fn project_strategy() -> impl Strategy<Value = Project> {
    prop_oneof![
        prop::collection::vec(module_strategy(), 26..36),
        prop::collection::vec(module_strategy(), 58..68),
    ]
    .prop_map(|modules| Project { modules })
}

fn name(index: usize) -> String {
    format!("mod_{index:03}")
}

fn write(root: &Path, file: &str, text: &str) {
    let path = root.join(file);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("mkdir");
    }
    fs::write(path, text).expect("write");
}

impl Project {
    /// The modules `index` calls into: earlier ones only, so the graph has
    /// no cycle.
    fn imports(&self, index: usize) -> BTreeSet<usize> {
        if index == 0 {
            return BTreeSet::new();
        }
        self.modules[index]
            .imports
            .iter()
            .map(|pick| pick % index)
            .collect()
    }

    /// Writes the project with module `index` named `rename(index)`.
    fn render(&self, rename: &dyn Fn(usize) -> String) -> TempDir {
        let dir = TempDir::new().expect("tempdir");
        let root = dir.path();
        let deps = POOL
            .iter()
            .map(|dep| format!("\"{dep}\""))
            .collect::<Vec<_>>()
            .join(", ");
        write(
            root,
            "pyproject.toml",
            &format!(
                "[project]\nname = \"pkg\"\nversion = \"0.1.0\"\ndependencies = [{deps}]\n\n[project.scripts]\npkg = \"pkg.main:main\"\n\n[tool.chokkin]\nmode = \"app\"\n"
            ),
        );
        write(root, "src/pkg/__init__.py", "");
        let mut main = String::new();
        let mut calls = String::new();
        for (index, module) in self.modules.iter().enumerate() {
            if module.from_main {
                writeln!(main, "from pkg.{} import used_{index}", rename(index)).expect("fmt");
                writeln!(calls, "    used_{index}()").expect("fmt");
            }
        }
        write!(main, "\n\ndef main():\n{calls}    pass\n").expect("fmt");
        write(root, "src/pkg/main.py", &main);
        for (index, module) in self.modules.iter().enumerate() {
            let mut text = String::new();
            for dep in &module.deps {
                writeln!(text, "import {}", POOL[*dep].replace('-', "_")).expect("fmt");
            }
            let imports = self.imports(index);
            for import in &imports {
                writeln!(text, "from pkg.{} import used_{import}", rename(*import)).expect("fmt");
            }
            write!(text, "\n\ndef used_{index}():\n").expect("fmt");
            for import in &imports {
                writeln!(text, "    used_{import}()").expect("fmt");
            }
            write!(text, "    pass\n\n\ndef unused_{index}():\n    pass\n").expect("fmt");
            write(root, &format!("src/pkg/{}.py", rename(index)), &text);
        }
        dir
    }
}

/// `(code, target, severity, confidence, line)` of every issue.
type Key = (String, String, String, String, Option<u64>);

fn issues(root: &Path) -> BTreeSet<Key> {
    let output = Command::new(env!("CARGO_BIN_EXE_chokkin"))
        .args(["--reporter", "json", "--no-cache"])
        .arg(root)
        .output()
        .expect("run chokkin");
    assert!(
        output.status.code().is_some_and(|code| code <= 1),
        "chokkin failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let parsed: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("json reporter output");
    parsed["issues"]
        .as_array()
        .expect("issues array")
        .iter()
        .map(|issue| {
            let field = |name: &str| issue[name].as_str().unwrap_or_default().to_owned();
            (
                field("code"),
                field("target"),
                field("severity"),
                field("confidence"),
                issue["line"].as_u64(),
            )
        })
        .collect()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(chokkin::internals::proptest_cases(12)))]

    /// CHK001 lists the modules `main` does not reach, on one worker or
    /// several, and a second run reports the same.
    #[test]
    fn unused_files_match_reachability(project in project_strategy()) {
        let mut reachable = BTreeSet::new();
        let mut stack: Vec<usize> = (0..project.modules.len())
            .filter(|index| project.modules[*index].from_main)
            .collect();
        while let Some(index) = stack.pop() {
            if reachable.insert(index) {
                stack.extend(project.imports(index));
            }
        }
        let expected: BTreeSet<String> = (0..project.modules.len())
            .filter(|index| !reachable.contains(index))
            .map(|index| format!("src/pkg/{}.py", name(index)))
            .collect();
        let dir = project.render(&name);
        let found = issues(dir.path());
        let unused: BTreeSet<String> = found
            .iter()
            .filter(|(code, ..)| code == "CHK001")
            .map(|(_, target, ..)| target.clone())
            .collect();
        prop_assert_eq!(unused, expected);
        prop_assert_eq!(issues(dir.path()), found);
    }

    /// A new file nothing imports changes no finding outside it, whatever
    /// it imports itself.
    #[test]
    fn an_unreferenced_file_adds_only_its_own_findings(
        project in project_strategy(),
        extra in 1usize..6,
        imports in prop::collection::vec(any::<usize>(), 0..4),
        deps in prop::collection::vec(0..POOL.len(), 0..3),
    ) {
        let dir = project.render(&name);
        let before = issues(dir.path());
        let mut text = String::new();
        for dep in &deps {
            writeln!(text, "import {}", POOL[*dep].replace('-', "_")).expect("fmt");
        }
        for pick in &imports {
            let index = pick % project.modules.len();
            writeln!(text, "from pkg.{} import unused_{index}\nunused_{index}()", name(index)).expect("fmt");
        }
        text.push_str("\n\ndef helper():\n    pass\n");
        let added: Vec<String> = (0..extra).map(|index| format!("src/pkg/zz_new_{index}.py")).collect();
        for file in &added {
            write(dir.path(), file, &text);
        }
        let after = issues(dir.path());
        let outside: BTreeSet<Key> = after
            .iter()
            .filter(|(_, target, ..)| !added.iter().any(|file| target.starts_with(file.as_str())))
            .cloned()
            .collect();
        prop_assert_eq!(outside, before);
        for file in &added {
            prop_assert!(
                after.iter().any(|(code, target, ..)| code == "CHK001" && target == file),
                "{file} is not CHK001: {after:?}"
            );
        }
    }

    /// Renaming modules everywhere moves their findings to the new names.
    #[test]
    fn renaming_modules_moves_their_findings(
        project in project_strategy(),
        renamed in prop::collection::vec(any::<prop::sample::Index>(), 1..4),
    ) {
        let renamed: BTreeSet<usize> = renamed.iter().map(|pick| pick.index(project.modules.len())).collect();
        let rename = |index: usize| {
            if renamed.contains(&index) { format!("ren_{index:03}") } else { name(index) }
        };
        let before = issues(project.render(&name).path());
        let after = issues(project.render(&rename).path());
        let moved: BTreeSet<Key> = before
            .into_iter()
            .map(|(code, mut target, severity, confidence, line)| {
                for index in &renamed {
                    target = target.replace(&format!("src/pkg/{}.py", name(*index)), &format!("src/pkg/{}.py", rename(*index)));
                }
                (code, target, severity, confidence, line)
            })
            .collect();
        prop_assert_eq!(after, moved);
    }
}
