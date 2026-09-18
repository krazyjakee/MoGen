//! What the import carried over, and everything it did not.
//!
//! The report is written into the emitted file's header, so it is the durable
//! record of every lossy decision. Nothing is dropped silently: a reader that
//! meets something the IR has no place for calls [`Report::skip`] or
//! [`Report::note`].

use crate::ir::MeshScene;

#[derive(Debug, Default, Clone)]
pub struct Report {
    pub nodes: usize,
    pub meshes: usize,
    pub primitives: usize,
    pub triangles: usize,
    pub materials: usize,
    pub textures: usize,
    /// `(what, count)` in the order each kind was first met. A `Vec` rather
    /// than a map so the header lists them in source order, reproducibly.
    pub skipped: Vec<(String, usize)>,
    /// Free-form, one line each: approximations and anything a reader wants
    /// the user to look at.
    pub notes: Vec<String>,
}

impl Report {
    /// Count one more `what` that could not be imported.
    pub fn skip(&mut self, what: &str) {
        self.skip_n(what, 1);
    }

    pub fn skip_n(&mut self, what: &str, n: usize) {
        if n == 0 {
            return;
        }
        match self.skipped.iter_mut().find(|(k, _)| k == what) {
            Some((_, count)) => *count += n,
            None => self.skipped.push((what.to_string(), n)),
        }
    }

    /// Record a line for the user, once — readers call this per primitive and
    /// would otherwise repeat the same approximation hundreds of times.
    pub fn note(&mut self, line: impl Into<String>) {
        let line = line.into();
        if !self.notes.contains(&line) {
            self.notes.push(line);
        }
    }

    /// Fill in the totals from what actually survived conversion.
    pub(crate) fn tally(&mut self, scene: &MeshScene) {
        self.nodes = scene.nodes.len();
        self.meshes = scene.meshes.len();
        self.primitives = scene.meshes.iter().map(|m| m.primitives.len()).sum();
        self.triangles = scene
            .meshes
            .iter()
            .flat_map(|m| &m.primitives)
            .map(|p| p.triangle_count())
            .sum();
        self.materials = scene.materials.len();
        self.textures = crate::emit::referenced_assets(scene).len();
    }

    /// The lines the header comment and the CLI both print.
    pub fn summary(&self) -> Vec<String> {
        let mut out = vec![format!(
            "{} nodes · {} meshes · {} polys · {} triangles · {} materials · {} textures",
            self.nodes, self.meshes, self.primitives, self.triangles, self.materials, self.textures
        )];
        if !self.skipped.is_empty() {
            let detail: Vec<String> = self
                .skipped
                .iter()
                .map(|(k, n)| format!("{k} ({n})"))
                .collect();
            out.push(format!("Skipped: {}", detail.join(", ")));
        }
        out.extend(self.notes.iter().cloned());
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skips_accumulate_in_first_seen_order() {
        let mut r = Report::default();
        r.skip("skins");
        r.skip("cameras");
        r.skip("skins");
        r.skip_n("lights", 0);
        assert_eq!(r.skipped, vec![("skins".into(), 2), ("cameras".into(), 1)]);
    }

    #[test]
    fn notes_are_deduplicated() {
        let mut r = Report::default();
        r.note("a");
        r.note("a");
        assert_eq!(r.notes, vec!["a".to_string()]);
    }
}
