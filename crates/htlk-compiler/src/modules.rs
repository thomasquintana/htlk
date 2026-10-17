//! Exact module import traversal; no directory or package-name searching.
use crate::{
    Compiler, DiagnosticLocation, MCPCatalogInputs, SourceSpan,
    ast::{DeclKind, Module},
    bundle::{ModuleRef, SourceBundle, module_path},
    diagnostic::{Result, fail},
};
use std::collections::BTreeMap;

pub(crate) fn session<'a>(
    c: &'a Compiler,
    b: &SourceBundle,
    target: &ModuleRef,
    catalogs: &'a MCPCatalogInputs,
    inspection: bool,
) -> Result<(crate::lower::Session<'a>, usize)> {
    b.validate(c)?;
    let mut traversal = Traversal {
        c,
        b,
        modules: vec![],
        imports: vec![],
        indices: BTreeMap::new(),
        active: BTreeMap::new(),
        tokens: 0,
        heights: vec![],
    };
    let root = traversal.visit(target, false, inspection, None)?;
    Ok((
        crate::lower::Session::new(c, catalogs, traversal.modules, traversal.imports)?,
        root,
    ))
}
struct Traversal<'a> {
    c: &'a Compiler,
    b: &'a SourceBundle,
    modules: Vec<Module>,
    imports: Vec<BTreeMap<String, usize>>,
    indices: BTreeMap<ModuleRef, usize>,
    active: BTreeMap<ModuleRef, Option<SourceSpan>>,
    tokens: usize,
    heights: Vec<usize>,
}
impl Traversal<'_> {
    fn visit(
        &mut self,
        r: &ModuleRef,
        imported: bool,
        inspection: bool,
        site: Option<SourceSpan>,
    ) -> Result<usize> {
        if self.active.contains_key(r) {
            let mut e = fail("C_IMPORT_CYCLE", "module import cycle");
            e.diagnostics[0].related_locations = self
                .active
                .values()
                .filter_map(|s| s.clone().map(DiagnosticLocation::SourceSpan))
                .collect();
            if let Some(site) = site {
                e.diagnostics[0].location = DiagnosticLocation::SourceSpan(site);
            }
            return Err(e);
        }
        if let Some(i) = self.indices.get(r) {
            if self.active.len().saturating_add(self.heights[*i]) > self.c.config.frontend.max_depth
            {
                return Err(fail("C_SOURCE_LIMIT", "module import depth exceeded"));
            }
            if imported
                && self.modules[*i]
                    .declarations
                    .iter()
                    .any(|d| matches!(d.kind, DeclKind::Graph(_)))
            {
                return Err(fail("C_IMPORT", "imported module contains a graph"));
            }
            return Ok(*i);
        }
        if self.active.len() >= self.c.config.frontend.max_depth {
            return Err(fail("C_SOURCE_LIMIT", "module import depth exceeded"));
        }
        let (source, location) = self.b.module(r)?;
        let module = crate::parser::parse(
            source,
            location,
            self.c.config.frontend.max_tokens - self.tokens,
            self.c.config.frontend.max_depth,
            &self.c.config.limits,
        )?;
        self.tokens += module.tokens;
        let graphs = module
            .declarations
            .iter()
            .filter(|d| matches!(d.kind, DeclKind::Graph(_)))
            .count();
        if graphs > 1 || (!inspection && !imported && graphs != 1) || (imported && graphs != 0) {
            return Err(fail(
                "C_IMPORT",
                "module graph cardinality is invalid for its compilation role",
            ));
        }
        self.active.insert(r.clone(), site);
        let mut imports = BTreeMap::new();
        for import in &module.imports {
            let (alias, path) = &import.value;
            let result = (|| {
                let (package, module) = path
                    .split_once('/')
                    .filter(|(_, m)| module_path(m))
                    .ok_or_else(|| fail("C_IMPORT", "invalid exact import path"))?;
                let package_digest = if package == "self" {
                    r.package_digest
                } else {
                    *self.b.packages[&r.package_digest]
                        .manifest
                        .dependencies
                        .get(package)
                        .ok_or_else(|| {
                            fail("C_IMPORT", "import requires a direct dependency pin")
                        })?
                };
                let target = ModuleRef {
                    package_digest,
                    module_path: module.into(),
                };
                let index = self.visit(&target, true, inspection, Some(import.span.clone()))?;
                if imports.insert(alias.clone(), index).is_some() {
                    return Err(fail("C_DUPLICATE", "duplicate import alias"));
                }
                Ok(())
            })();
            result.map_err(|mut e|{for d in &mut e.diagnostics{if matches!(&d.location,DiagnosticLocation::CanonicalRecordPath(p) if p.is_empty()){d.location=DiagnosticLocation::SourceSpan(import.span.clone());}}e})?;
        }
        self.active.remove(r);
        let height = 1 + imports
            .values()
            .map(|i| self.heights[*i])
            .max()
            .unwrap_or(0);
        if height > self.c.config.frontend.max_depth {
            return Err(fail("C_SOURCE_LIMIT", "module import depth exceeded"));
        }
        let index = self.modules.len();
        self.heights.push(height);
        self.modules.push(module);
        self.imports.push(imports);
        self.indices.insert(r.clone(), index);
        Ok(index)
    }
}
impl Compiler {
    /// Compiles an exact finite source bundle's entry module and import closure.
    ///
    /// # Errors
    /// Returns snapshot, pin, import/visibility, source or semantic diagnostics.
    pub fn compile_bundle(
        &self,
        bundle: &SourceBundle,
        catalogs: &MCPCatalogInputs,
    ) -> Result<crate::CompilationSuccess> {
        let (mut session, root) = session(self, bundle, &bundle.entry, catalogs, false)?;
        session.finish(root)
    }
}
