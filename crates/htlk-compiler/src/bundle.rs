//! Closed finite source snapshots and exact source-package identities.
use crate::{
    Compiler, DiagnosticLocation, SourceLocation,
    diagnostic::{Result, fail},
    executable::{
        Identifier, JsonDocument,
        cbor::{self, Limits, Map, Value},
        digest::{Digest, hash_bytes},
    },
    input_value::*,
};
use std::collections::{BTreeMap, BTreeSet};

/// Exact supplied module selection.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ModuleRef {
    /// Recomputed source package identity.
    pub package_digest: Digest,
    /// Exact manifest module key.
    pub module_path: String,
}
/// Compiler input manifest; format version is always 0.1.
#[derive(Clone, Debug)]
pub struct SourcePackageManifest {
    /// Qualified snake_case descriptive package name.
    pub package_name: String,
    /// Nonempty descriptive version, without range selection.
    pub package_version: String,
    /// Logical module paths to exact source paths.
    pub modules: BTreeMap<String, String>,
    /// Direct package aliases to exact supplied identities.
    pub dependencies: BTreeMap<String, Digest>,
}
/// One immutable-by-request source snapshot.
#[derive(Clone, Debug)]
pub struct SourcePackage {
    /// Exact manifest.
    pub manifest: SourcePackageManifest,
    /// Source paths to complete decoded source strings.
    pub sources: BTreeMap<String, String>,
}
/// Closed supplied package dependency closure. No source discovery is performed.
#[derive(Clone, Debug)]
pub struct SourceBundle {
    /// Selected compilation entry module.
    pub entry: ModuleRef,
    /// Exact package identities and contents.
    pub packages: BTreeMap<Digest, SourcePackage>,
}
fn object(entries: impl IntoIterator<Item = (String, Value)>) -> Result<Value> {
    Ok(Value::Map(
        Map::try_from_entries(entries).map_err(|e| fail("C_SOURCE_BUNDLE", e))?,
    ))
}
impl SourcePackage {
    /// Computes the exact source identity, including comments and line endings.
    ///
    /// # Errors
    /// Returns bounded CBOR construction or encoding failures.
    pub fn digest(&self, l: &Limits) -> Result<Digest> {
        let m = &self.manifest;
        let mut size = m
            .package_name
            .len()
            .saturating_add(m.package_version.len())
            .saturating_add(m.dependencies.len().saturating_mul(80));
        for (k, v) in m.modules.iter().chain(self.sources.iter()) {
            size = size
                .saturating_add(k.len())
                .saturating_add(v.len())
                .saturating_add(2);
        }
        if size > l.max_document_bytes {
            return Err(fail(
                "C_SOURCE_LIMIT",
                "source package exceeds digest construction limit",
            ));
        }
        let manifest = object([
            ("manifest_version".into(), Value::Text("0.1".into())),
            ("package_name".into(), Value::Text(m.package_name.clone())),
            (
                "package_version".into(),
                Value::Text(m.package_version.clone()),
            ),
            (
                "modules".into(),
                object(
                    m.modules
                        .iter()
                        .map(|(k, v)| (k.clone(), Value::Text(v.clone()))),
                )?,
            ),
            (
                "dependencies".into(),
                object(
                    m.dependencies
                        .iter()
                        .map(|(k, v)| (k.clone(), Value::Text(v.to_string()))),
                )?,
            ),
        ])?;
        let sources = object(
            self.sources
                .iter()
                .map(|(k, v)| (k.clone(), Value::Text(v.clone()))),
        )?;
        let bytes = cbor::encode(
            &Value::Array(vec![
                Value::Text("htlk.source_package".into()),
                Value::Text("0.1".into()),
                manifest,
                sources,
            ]),
            l,
        )
        .map_err(|e| fail("C_SOURCE_LIMIT", e))?;
        Ok(hash_bytes(&bytes))
    }
}
impl SourceBundle {
    /// Parses duplicate-aware JSON records without filesystem or registry access.
    /// Full snapshot validation is performed by every compiler request.
    ///
    /// # Errors
    /// Rejects duplicate/unknown fields, invalid versions and malformed record data.
    pub fn from_json(bytes: &[u8], l: &Limits) -> Result<Self> {
        let doc = JsonDocument::new(bytes, l)
            .map_err(|e| bundle_error("C_SOURCE_BUNDLE", &e.to_string(), ""))?;
        Self::parse(doc.value()).map_err(|mut e| {
            for d in &mut e.diagnostics {
                if d.code == "C_SYNTAX" {
                    d.code = "C_SOURCE_BUNDLE".into();
                }
                d.location = DiagnosticLocation::SourceBundlePath(String::new());
            }
            e
        })
    }
    fn parse(v: &Value) -> Result<Self> {
        let b = closed(v, &["format", "format_version", "entry", "packages"], &[])?;
        if text(field(b, "format")?)? != "htlk_source_bundle"
            || text(field(b, "format_version")?)? != "0.1"
        {
            return Err(fail(
                "C_VERSION",
                "unsupported source bundle format/version",
            ));
        }
        let entry = closed(field(b, "entry")?, &["package_digest", "module_path"], &[])?;
        let entry = ModuleRef {
            package_digest: text(field(entry, "package_digest")?)?
                .parse()
                .map_err(|e| fail("C_PACKAGE_PIN", e))?,
            module_path: text(field(entry, "module_path")?)?.into(),
        };
        let mut packages = BTreeMap::new();
        for (pin, p) in map(field(b, "packages")?)?.iter() {
            let p = closed(p, &["manifest", "sources"], &[])?;
            let m = closed(
                field(p, "manifest")?,
                &[
                    "manifest_version",
                    "package_name",
                    "package_version",
                    "modules",
                    "dependencies",
                ],
                &[],
            )?;
            if text(field(m, "manifest_version")?)? != "0.1" {
                return Err(fail("C_VERSION", "unsupported source manifest version"));
            }
            let modules = map(field(m, "modules")?)?
                .iter()
                .map(|(k, v)| Ok((k.into(), text(v)?.into())))
                .collect::<Result<_>>()?;
            let dependencies = map(field(m, "dependencies")?)?
                .iter()
                .map(|(k, v)| {
                    Ok((
                        k.into(),
                        text(v)?.parse().map_err(|e| fail("C_PACKAGE_PIN", e))?,
                    ))
                })
                .collect::<Result<_>>()?;
            let sources = map(field(p, "sources")?)?
                .iter()
                .map(|(k, v)| Ok((k.into(), text(v)?.into())))
                .collect::<Result<_>>()?;
            packages.insert(
                pin.parse().map_err(|e| fail("C_PACKAGE_PIN", e))?,
                SourcePackage {
                    manifest: SourcePackageManifest {
                        package_name: text(field(m, "package_name")?)?.into(),
                        package_version: text(field(m, "package_version")?)?.into(),
                        modules,
                        dependencies,
                    },
                    sources,
                },
            );
        }
        Ok(Self { entry, packages })
    }
    pub(crate) fn validate(&self, c: &Compiler) -> Result<()> {
        let f = &c.config.frontend;
        if self.packages.len() > f.max_packages {
            return Err(bundle_error(
                "C_SOURCE_LIMIT",
                "package count limit exceeded",
                "/packages",
            ));
        }
        let mut bytes = 0usize;
        let mut modules = 0usize;
        for (pin, p) in &self.packages {
            let path = format!("/packages/{pin}");
            let check = (|| {
                let m = &p.manifest;
                if m.package_version.is_empty()
                    || m.package_name
                        .split('.')
                        .any(|p| p.parse::<Identifier>().is_err())
                {
                    return Err(fail("C_SOURCE_BUNDLE", "invalid package name/version"));
                }
                modules = modules
                    .checked_add(m.modules.len())
                    .ok_or_else(|| fail("C_SOURCE_LIMIT", "module count overflow"))?;
                if m.modules.is_empty() || modules > f.max_modules {
                    return Err(fail(
                        "C_SOURCE_LIMIT",
                        "invalid/excessive mapped module count",
                    ));
                }
                let paths = m.modules.values().collect::<BTreeSet<_>>();
                if paths.len() != m.modules.len()
                    || paths != p.sources.keys().collect::<BTreeSet<_>>()
                {
                    return Err(fail(
                        "C_SOURCE_BUNDLE",
                        "mapped source paths must uniquely equal supplied source keys",
                    ));
                }
                for (module, source_path) in &m.modules {
                    if !module_path(module) || !source_path_valid(source_path) {
                        return Err(fail("C_SOURCE_BUNDLE", "invalid module/source path"));
                    }
                    let source = &p.sources[source_path];
                    bytes = bytes
                        .checked_add(source.len())
                        .ok_or_else(|| fail("C_SOURCE_LIMIT", "source byte overflow"))?;
                    if bytes > f.max_source_bytes {
                        return Err(fail("C_SOURCE_LIMIT", "source byte limit exceeded"));
                    }
                    crate::lexer::check_header(
                        source,
                        SourceLocation::Module {
                            package_digest: *pin,
                            module_path: module.clone(),
                            source_path: source_path.clone(),
                        },
                        &c.config.limits,
                    )?;
                }
                for alias in m.dependencies.keys() {
                    if alias == "self" || alias.parse::<Identifier>().is_err() {
                        return Err(fail("C_SOURCE_BUNDLE", "invalid dependency alias"));
                    }
                }
                if p.digest(&c.config.limits)? != *pin {
                    return Err(fail(
                        "C_PACKAGE_PIN",
                        "package content does not match its pin",
                    ));
                }
                Ok(())
            })();
            check.map_err(|mut e| {
                for d in &mut e.diagnostics {
                    if matches!(d.location, DiagnosticLocation::CanonicalRecordPath(_)) {
                        d.location = DiagnosticLocation::SourceBundlePath(path.clone());
                    }
                }
                e
            })?;
        }
        let mut active = BTreeSet::new();
        let mut reached = BTreeMap::new();
        self.package_visit(self.entry.package_digest, 0, c, &mut active, &mut reached)?;
        if reached.len() != self.packages.len() {
            return Err(bundle_error(
                "C_SOURCE_BUNDLE",
                "bundle has packages outside entry dependency closure",
                "/packages",
            ));
        }
        self.module(&self.entry)?;
        Ok(())
    }
    fn package_visit(
        &self,
        pin: Digest,
        depth: usize,
        c: &Compiler,
        active: &mut BTreeSet<Digest>,
        reached: &mut BTreeMap<Digest, usize>,
    ) -> Result<usize> {
        if depth >= c.config.frontend.max_depth {
            return Err(bundle_error(
                "C_SOURCE_LIMIT",
                "package dependency depth exceeded",
                "/packages",
            ));
        }
        if active.contains(&pin) {
            let mut e = bundle_error(
                "C_SOURCE_BUNDLE",
                "package dependency cycle",
                &format!("/packages/{pin}/manifest/dependencies"),
            );
            e.diagnostics[0].related_locations = active
                .iter()
                .map(|pin| {
                    DiagnosticLocation::SourceBundlePath(format!(
                        "/packages/{pin}/manifest/dependencies"
                    ))
                })
                .collect();
            return Err(e);
        }
        if let Some(height) = reached.get(&pin) {
            if depth.saturating_add(*height) > c.config.frontend.max_depth {
                return Err(bundle_error(
                    "C_SOURCE_LIMIT",
                    "package dependency depth exceeded",
                    "/packages",
                ));
            }
            return Ok(*height);
        }
        let p = self.packages.get(&pin).ok_or_else(|| {
            bundle_error(
                "C_PACKAGE_PIN",
                "missing selected package",
                "/entry/package_digest",
            )
        })?;
        active.insert(pin);
        let mut height = 1;
        for (alias, next) in &p.manifest.dependencies {
            if !self.packages.contains_key(next) {
                return Err(bundle_error(
                    "C_PACKAGE_PIN",
                    "missing selected dependency package",
                    &format!("/packages/{pin}/manifest/dependencies/{alias}"),
                ));
            }
            height = height.max(1 + self.package_visit(*next, depth + 1, c, active, reached)?);
        }
        active.remove(&pin);
        reached.insert(pin, height);
        Ok(height)
    }
    pub(crate) fn module(&self, r: &ModuleRef) -> Result<(&str, SourceLocation)> {
        let p = self
            .packages
            .get(&r.package_digest)
            .ok_or_else(|| bundle_error("C_PACKAGE_PIN", "missing package", "/packages"))?;
        let path = p.manifest.modules.get(&r.module_path).ok_or_else(|| {
            bundle_error(
                "C_IMPORT",
                "missing exact module key",
                &format!("/packages/{}/manifest/modules", r.package_digest),
            )
        })?;
        let source = p
            .sources
            .get(path)
            .ok_or_else(|| bundle_error("C_SOURCE_BUNDLE", "missing mapped source", "/packages"))?;
        Ok((
            source,
            SourceLocation::Module {
                package_digest: r.package_digest,
                module_path: r.module_path.clone(),
                source_path: path.clone(),
            },
        ))
    }
}
pub(crate) fn module_path(s: &str) -> bool {
    !s.is_empty() && s.split('/').all(|p| p.parse::<Identifier>().is_ok())
}
fn source_path_valid(s: &str) -> bool {
    s.strip_suffix(".htlk").is_some_and(module_path)
}
pub(crate) fn bundle_error(code: &str, message: &str, path: &str) -> crate::CompilationFailure {
    let mut e = fail(code, message);
    e.diagnostics[0].location = DiagnosticLocation::SourceBundlePath(path.into());
    e
}
