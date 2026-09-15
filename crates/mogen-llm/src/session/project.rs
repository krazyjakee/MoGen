use super::SessionLimits;
use crate::{GenerateConfig, ImageInput, Usage};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub fn identity(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelingBrief {
    pub revision: u64,
    pub prompt: String,
    pub corrections: Vec<String>,
    pub style: String,
    pub dimensions: String,
    pub intended_use: String,
    pub required_details: String,
    pub constraints: String,
    pub references: Vec<ReferenceImage>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReferenceImage {
    pub label: String,
    pub digest: String,
    pub image: ImageInput,
}
impl ModelingBrief {
    pub fn add_reference(&mut self, label: String, image: ImageInput) {
        let digest = identity(&image.data);
        if !self.references.iter().any(|r| r.digest == digest) {
            self.references.push(ReferenceImage {
                label,
                digest,
                image,
            });
            self.revision += 1;
        }
    }
    pub fn context(&self) -> String {
        format!("Original target: {}\nStyle: {}\nDimensions/units: {}\nIntended use: {}\nRequired details: {}\nApproved constraints: {}\nUser corrections: {}\nOriginal target image labels (in order): {}",
            self.prompt, self.style, self.dimensions, self.intended_use, self.required_details,
            self.constraints, self.corrections.join("\n"), self.references.iter().map(|r|r.label.as_str()).collect::<Vec<_>>().join(", "))
    }
    pub fn attach(&self, cfg: &mut GenerateConfig) -> Result<()> {
        for r in &self.references {
            if r.image.data.is_empty() || identity(&r.image.data) != r.digest {
                bail!("Missing or corrupt reference image: {}", r.label);
            }
            if !cfg.user_images.iter().any(|i| i.data == r.image.data) {
                cfg.user_images.push(r.image.clone());
            }
        }
        cfg.user_prompt = format!("{}\n\n{}", self.context(), cfg.user_prompt);
        Ok(())
    }
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum QualityMode {
    #[default]
    Draft,
    Refined,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Candidate {
    pub revision: String,
    pub source: String,
    pub brief_revision: u64,
    pub provider: String,
    pub model: String,
    pub usage: Usage,
    pub findings: String,
    pub views: Vec<RenderedView>,
    pub dependencies: BTreeMap<PathBuf, Vec<u8>>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenderedView {
    pub label: String,
    pub revision: String,
    pub image: ImageInput,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LockKind {
    Subtree,
    Geometry,
    Transform,
    Material,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PartLock {
    pub name: String,
    pub kind: LockKind,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelingProject {
    pub version: u32,
    pub brief: ModelingBrief,
    pub mode: QualityMode,
    pub limits: SessionLimits,
    pub locks: Vec<PartLock>,
    pub selected_part: Option<String>,
    pub candidates: Vec<Candidate>,
    pub selected_candidate: Option<usize>,
    pub stop_reason: String,
    pub experimental_guidance: bool,
}
impl Default for ModelingProject {
    fn default() -> Self {
        Self {
            version: 1,
            brief: ModelingBrief::default(),
            mode: QualityMode::Draft,
            limits: SessionLimits::default(),
            locks: vec![],
            selected_part: None,
            candidates: vec![],
            selected_candidate: None,
            stop_reason: String::new(),
            experimental_guidance: false,
        }
    }
}
impl ModelingProject {
    pub fn sidecar(path: &Path) -> PathBuf {
        let mut name = path.as_os_str().to_os_string();
        name.push(".modeling.json");
        PathBuf::from(name)
    }
    pub fn load(path: &Path) -> Result<Self> {
        let path = Self::sidecar(path);
        if !path.exists() {
            return Ok(Self::default());
        }
        let project: Self = serde_json::from_slice(&std::fs::read(&path)?)?;
        if project.version != 1 {
            bail!("Unsupported modeling session version {}", project.version);
        }
        if project
            .selected_candidate
            .is_some_and(|i| i >= project.candidates.len())
        {
            bail!("Invalid selected candidate in modeling session");
        }
        for candidate in &project.candidates {
            if revision(&candidate.source, &candidate.dependencies) != candidate.revision {
                bail!("Corrupt candidate snapshot");
            }
            if candidate
                .views
                .iter()
                .any(|view| view.revision != candidate.revision)
            {
                bail!("Stale render revision in candidate snapshot");
            }
        }
        for r in &project.brief.references {
            if identity(&r.image.data) != r.digest {
                bail!("Corrupt reference: {}", r.label);
            }
        }
        Ok(project)
    }
    pub fn save(&self, path: &Path) -> Result<()> {
        let path = Self::sidecar(path);
        let mut temp = tempfile::NamedTempFile::new_in(
            path.parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new(".")),
        )?;
        use std::io::Write;
        temp.write_all(&serde_json::to_vec(self)?)?;
        temp.as_file().sync_all()?;
        temp.persist(&path)
            .map_err(|e| e.error)
            .with_context(|| format!("Save modeling session {}", path.display()))?;
        Ok(())
    }
    pub fn record(
        &mut self,
        source: String,
        base: Option<&Path>,
        cfg: &GenerateConfig,
        provider: &str,
        findings: String,
        views: Vec<RenderedView>,
    ) -> Result<usize> {
        let dependencies = dependencies(&source, base)?;
        let revision = revision(&source, &dependencies);
        let candidate = Candidate {
            revision,
            source,
            brief_revision: self.brief.revision,
            provider: provider.into(),
            model: cfg.model.clone(),
            usage: cfg
                .session_control
                .as_ref()
                .map(|c| c.meter().usage)
                .unwrap_or_default(),
            findings,
            views,
            dependencies,
        };
        self.candidates.push(candidate);
        Ok(self.candidates.len() - 1)
    }
}
pub fn revision(source: &str, dependencies: &BTreeMap<PathBuf, Vec<u8>>) -> String {
    let mut bytes = source.as_bytes().to_vec();
    for (p, b) in dependencies {
        bytes.extend_from_slice(p.to_string_lossy().as_bytes());
        bytes.extend_from_slice(identity(b).as_bytes());
    }
    identity(&bytes)
}
/// Capture referenced source, textures and binary meshes inside the project.
/// Files are read-only during AI sessions, so restoring a source never rewrites another asset.
pub fn dependencies(source: &str, base: Option<&Path>) -> Result<BTreeMap<PathBuf, Vec<u8>>> {
    fn visit(
        src: &str,
        dir: &Path,
        root: &Path,
        out: &mut BTreeMap<PathBuf, Vec<u8>>,
    ) -> Result<()> {
        fn walk(nodes: &[mogen_dsl::ast::Node], paths: &mut Vec<(String, bool)>) {
            for n in nodes {
                if n.kind == "import" {
                    if let Some(name) = &n.name {
                        paths.push((name.clone(), true));
                    }
                }
                for (key, value) in &n.attrs {
                    if key.contains("texture") || (n.kind == "mesh" && key == "src") {
                        if let mogen_dsl::ast::Value::String(p)
                        | mogen_dsl::ast::Value::Ident(p) = value
                        {
                            paths.push((p.clone(), false));
                        }
                    }
                }
                walk(&n.children, paths);
            }
        }
        let ast = mogen_dsl::parse(src)?;
        let mut paths = vec![];
        walk(&ast, &mut paths);
        for (p, import) in paths {
            let path = dir
                .join(&p)
                .canonicalize()
                .with_context(|| format!("Missing dependency {p}"))?;
            if !path.starts_with(root) {
                bail!(
                    "Dependency outside the modeling project: {}",
                    path.display()
                );
            }
            let key = path.strip_prefix(root)?.to_path_buf();
            if out.contains_key(&key) {
                continue;
            }
            let bytes = std::fs::read(&path)?;
            out.insert(key, bytes.clone());
            if import {
                visit(
                    std::str::from_utf8(&bytes)?,
                    path.parent().unwrap(),
                    root,
                    out,
                )?;
            }
        }
        Ok(())
    }
    let mut out = BTreeMap::new();
    let root = base.unwrap_or(Path::new(".")).canonicalize()?;
    visit(source, &root, &root, &mut out)?;
    Ok(out)
}

impl Candidate {
    /// Recover a self-contained revision in a new directory. Existing project
    /// files are never replaced, including externally edited shared materials.
    pub fn restore_copy(&self, destination: &Path) -> Result<PathBuf> {
        if destination.exists() {
            bail!("Restore destination already exists");
        }
        if revision(&self.source, &self.dependencies) != self.revision {
            bail!("Candidate snapshot is corrupt");
        }
        for path in self.dependencies.keys() {
            if path
                .components()
                .any(|c| !matches!(c, std::path::Component::Normal(_)))
            {
                bail!("Invalid snapshot dependency path");
            }
        }
        let parent = destination.parent().unwrap_or(Path::new("."));
        std::fs::create_dir_all(parent)?;
        let temp = tempfile::tempdir_in(parent)?;
        for (path, bytes) in &self.dependencies {
            let target = temp.path().join(path);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(target, bytes)?;
        }
        // Imported assets may already use the conventional entry-point name.
        // Preserve them and choose an unused root-level source filename.
        let mut entry = PathBuf::from("restored.mog");
        let mut suffix = 1;
        while temp.path().join(&entry).exists() {
            entry = PathBuf::from(format!("restored-{suffix}.mog"));
            suffix += 1;
        }
        std::fs::write(temp.path().join(&entry), &self.source)?;
        std::fs::rename(temp.path(), destination)?;
        Ok(destination.join(entry))
    }
}
