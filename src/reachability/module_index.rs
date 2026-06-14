//! First-party module name → file index.

use std::collections::HashMap;

use crate::VERSION;
use crate::cache::{
    CacheKeyContext, CacheOptions, ScanCacheKey, SourceFingerprint, stable_hex_hash,
};
use crate::graph::{FileId, ProjectGraph};
use crate::sources::{DiscoveredSources, LayoutInfo, ProjectLayout};

/// Maps dotted module names to project files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleIndex {
    module_to_file: HashMap<String, FileId>,
}

impl ModuleIndex {
    /// Build a module index from discovered sources and the project graph.
    #[must_use]
    pub fn build(graph: &ProjectGraph, sources: &DiscoveredSources) -> Self {
        let mut module_to_file = HashMap::new();
        for (file_id, file) in graph.files() {
            if let Some(module) = path_to_module(&file.path, &sources.layout) {
                module_to_file.entry(module).or_insert(file_id);
            }
        }
        Self { module_to_file }
    }

    /// Build a module index, optionally using the Phase 2 scan cache.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when reading source fingerprints or cache files fails.
    pub fn build_with_cache(
        graph: &ProjectGraph,
        sources: &DiscoveredSources,
        cache: Option<&CacheOptions>,
    ) -> Result<Self, std::io::Error> {
        let Some(cache) = cache.filter(|cache| cache.enabled) else {
            return Ok(Self::build(graph, sources));
        };
        let key = module_index_cache_key(sources)?;
        if let Some(payload) =
            cache.read_scan_payload::<ModuleIndexPayload>(sources.root.path.as_path(), &key)?
        {
            if let Some(index) = Self::from_payload(graph, &payload) {
                return Ok(index);
            }
        }

        let index = Self::build(graph, sources);
        cache.write_scan_payload(sources.root.path.as_path(), key, &index.to_payload(graph))?;
        Ok(index)
    }

    /// Resolve a dotted module name to a first-party file id.
    #[must_use]
    pub fn resolve(&self, module: &str) -> Option<FileId> {
        self.module_to_file.get(module).copied()
    }

    fn from_payload(graph: &ProjectGraph, payload: &ModuleIndexPayload) -> Option<Self> {
        let mut module_to_file = HashMap::new();
        for entry in &payload.entries {
            let file_id = graph.file_id(&entry.path)?;
            module_to_file.insert(entry.module.clone(), file_id);
        }
        Some(Self { module_to_file })
    }

    fn to_payload(&self, graph: &ProjectGraph) -> ModuleIndexPayload {
        let mut entries = Vec::new();
        for (module, file_id) in &self.module_to_file {
            if let Some(file) = graph.file(*file_id) {
                entries.push(ModuleIndexEntry {
                    module: module.clone(),
                    path: file.path.clone(),
                });
            }
        }
        entries.sort_by(|left, right| left.module.cmp(&right.module));
        ModuleIndexPayload { entries }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct ModuleIndexPayload {
    entries: Vec<ModuleIndexEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct ModuleIndexEntry {
    module: String,
    path: String,
}

fn module_index_cache_key(sources: &DiscoveredSources) -> Result<ScanCacheKey, std::io::Error> {
    let mut fingerprints = Vec::new();
    for file in &sources.files {
        fingerprints.push(SourceFingerprint::from_root_relative(
            sources.root.path.as_path(),
            &file.path,
        )?);
    }
    fingerprints.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(ScanCacheKey {
        context: CacheKeyContext {
            chokkin_version: VERSION.to_owned(),
            config_hash: stable_hex_hash(format!("{:?}", sources.layout).as_bytes()),
            manifest_hash: stable_hex_hash(format!("{:?}", sources.effective_globs).as_bytes()),
            target_version: "n/a".to_owned(),
            unit_version: "module-index-v1".to_owned(),
        },
        inputs: crate::cache::ScanInputFingerprints {
            config: Vec::new(),
            manifest: fingerprints,
        },
    })
}

/// Infer a dotted module name from a root-relative `.py` path.
#[must_use]
pub fn path_to_module(path: &str, layout: &LayoutInfo) -> Option<String> {
    let stem = path.strip_suffix(".py")?;
    let module_path = stem.strip_suffix("/__init__").unwrap_or(stem);

    match layout.layout {
        ProjectLayout::Src => module_path
            .strip_prefix("src/")
            .map(|rest| rest.replace('/', ".")),
        ProjectLayout::Flat => flat_module_name(module_path, layout),
        ProjectLayout::Unknown => module_path
            .strip_prefix("src/")
            .map(|rest| rest.replace('/', "."))
            .or_else(|| flat_module_name(module_path, layout)),
    }
}

fn flat_module_name(path: &str, layout: &LayoutInfo) -> Option<String> {
    for package in &layout.packages {
        if path == *package {
            return Some(package.clone());
        }
        if let Some(suffix) = path.strip_prefix(&format!("{package}/")) {
            return Some(format!("{package}.{}", suffix.replace('/', ".")));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::CacheOptions;
    use crate::discovery::{ProjectRoot, RootMarker};
    use crate::graph::{FileNode, ProjectGraph};
    use crate::sources::{FileContext, FileKind};

    #[test]
    fn path_to_module_src_layout() {
        let layout = LayoutInfo {
            layout: ProjectLayout::Src,
            packages: vec!["acme".to_owned()],
            inferred_globs: Vec::new(),
            flat_candidates: Vec::new(),
            ambiguous_flat_resolution: false,
        };
        assert_eq!(
            path_to_module("src/acme/api/routes.py", &layout),
            Some("acme.api.routes".to_owned())
        );
        assert_eq!(
            path_to_module("src/acme/__init__.py", &layout),
            Some("acme".to_owned())
        );
    }

    #[test]
    fn module_index_resolves_registered_file() {
        let mut graph = ProjectGraph::new(ProjectRoot {
            path: std::env::temp_dir(),
            marker: RootMarker::PyProjectToml,
            start: std::env::temp_dir(),
        });
        let file_id = graph
            .intern_file(FileNode {
                path: "src/acme/foo.py".to_owned(),
                context: FileContext::Runtime,
                kind: FileKind::Python,
            })
            .expect("file");
        let sources = DiscoveredSources {
            root: graph.root.clone(),
            layout: LayoutInfo {
                layout: ProjectLayout::Src,
                packages: vec!["acme".to_owned()],
                inferred_globs: Vec::new(),
                flat_candidates: Vec::new(),
                ambiguous_flat_resolution: false,
            },
            effective_globs: Vec::new(),
            files: Vec::new(),
            warnings: Vec::new(),
        };
        let index = ModuleIndex::build(&graph, &sources);
        assert_eq!(index.resolve("acme.foo"), Some(file_id));
    }

    #[test]
    fn module_index_uses_disk_cache_payload() {
        let temp = tempfile::tempdir().expect("temp dir");
        let src = temp.path().join("src").join("acme");
        std::fs::create_dir_all(&src).expect("create src");
        std::fs::write(src.join("foo.py"), "").expect("write source");

        let root = ProjectRoot {
            path: temp.path().to_path_buf(),
            marker: RootMarker::PyProjectToml,
            start: temp.path().to_path_buf(),
        };
        let mut graph = ProjectGraph::new(root.clone());
        let file_id = graph
            .intern_file(FileNode {
                path: "src/acme/foo.py".to_owned(),
                context: FileContext::Runtime,
                kind: FileKind::Python,
            })
            .expect("file");
        let sources = DiscoveredSources {
            root,
            layout: LayoutInfo {
                layout: ProjectLayout::Src,
                packages: vec!["acme".to_owned()],
                inferred_globs: Vec::new(),
                flat_candidates: Vec::new(),
                ambiguous_flat_resolution: false,
            },
            effective_globs: vec!["src/**/*.py".to_owned()],
            files: vec![crate::sources::DiscoveredFile {
                path: "src/acme/foo.py".to_owned(),
                kind: FileKind::Python,
                context: FileContext::Runtime,
            }],
            warnings: Vec::new(),
        };
        let cache = CacheOptions::default();
        let first =
            ModuleIndex::build_with_cache(&graph, &sources, Some(&cache)).expect("first build");
        let second =
            ModuleIndex::build_with_cache(&graph, &sources, Some(&cache)).expect("cached build");
        assert_eq!(first.resolve("acme.foo"), Some(file_id));
        assert_eq!(second.resolve("acme.foo"), Some(file_id));
    }
}
