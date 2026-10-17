//! Binary graph composition through public ports using ordinary scope operations.
use crate::executable::cbor::Limits;
use crate::{
    CompilationSuccess, Compiler, MCPCatalogInputs,
    diagnostic::{Result, fail},
    executable::*,
    input_value::*,
};
use std::collections::{BTreeMap, BTreeSet};

/// One side of a binary composition.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum JoinOperand {
    /// Left canonical input.
    Left,
    /// Right canonical input.
    Right,
}
/// An operand's public port; never an internal node endpoint.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct JoinEndpoint {
    /// Selected operand.
    pub graph: JoinOperand,
    /// Public port name.
    pub port: String,
}
/// An authored cross-operand output-to-input binding.
#[derive(Clone, Debug)]
pub struct JoinEdge {
    /// Local edge ID.
    pub id: String,
    /// Operand public output.
    pub from: JoinEndpoint,
    /// Other operand public input.
    pub to: JoinEndpoint,
}
/// A composed public input and its whole-value fan-out destinations.
#[derive(Clone, Debug)]
pub struct JoinInput {
    /// New public input name.
    pub name: String,
    /// Nonempty operand-input destinations.
    pub to: Vec<JoinEndpoint>,
}
/// A composed public output.
#[derive(Clone, Debug)]
pub struct JoinOutput {
    /// New public output name.
    pub name: String,
    /// Selected operand public output.
    pub from: JoinEndpoint,
}
/// Closed binary graph composition request.
#[derive(Clone, Debug)]
pub struct JoinSpec {
    /// Qualified resulting graph ID.
    pub id: String,
    /// Left wrapper node ID.
    pub left_alias: String,
    /// Right wrapper node ID.
    pub right_alias: String,
    /// Cross-operand value edges.
    pub edges: Vec<JoinEdge>,
    /// Exposed composed inputs.
    pub inputs: Vec<JoinInput>,
    /// Exposed composed outputs.
    pub outputs: Vec<JoinOutput>,
}
impl JoinSpec {
    /// Parses bounded duplicate-aware JSON with closed request/endpoint shapes.
    ///
    /// # Errors
    /// Rejects unknown/duplicate fields, malformed endpoints and JSON limits.
    pub fn from_json(bytes: &[u8], limits: &Limits) -> Result<Self> {
        let doc = JsonDocument::new(bytes, limits).map_err(|e| fail("C_JOIN", e))?;
        Self::parse(doc.value()).map_err(|mut e| {
            for d in &mut e.diagnostics {
                d.code = "C_JOIN".into();
            }
            e
        })
    }
    fn parse(v: &cbor::Value) -> Result<Self> {
        let m = closed(
            v,
            &[
                "id",
                "left_alias",
                "right_alias",
                "edges",
                "inputs",
                "outputs",
            ],
            &[],
        )?;
        let mut edges = vec![];
        for v in array(field(m, "edges")?)? {
            let e = closed(v, &["id", "from", "to"], &[])?;
            edges.push(JoinEdge {
                id: text(field(e, "id")?)?.into(),
                from: endpoint(field(e, "from")?)?,
                to: endpoint(field(e, "to")?)?,
            });
        }
        let mut inputs = vec![];
        for v in array(field(m, "inputs")?)? {
            let e = closed(v, &["name", "to"], &[])?;
            inputs.push(JoinInput {
                name: text(field(e, "name")?)?.into(),
                to: array(field(e, "to")?)?
                    .iter()
                    .map(endpoint)
                    .collect::<Result<_>>()?,
            });
        }
        let mut outputs = vec![];
        for v in array(field(m, "outputs")?)? {
            let e = closed(v, &["name", "from"], &[])?;
            outputs.push(JoinOutput {
                name: text(field(e, "name")?)?.into(),
                from: endpoint(field(e, "from")?)?,
            });
        }
        Ok(Self {
            id: text(field(m, "id")?)?.into(),
            left_alias: text(field(m, "left_alias")?)?.into(),
            right_alias: text(field(m, "right_alias")?)?.into(),
            edges,
            inputs,
            outputs,
        })
    }
}
fn endpoint(v: &cbor::Value) -> Result<JoinEndpoint> {
    let m = closed(v, &["graph", "port"], &[])?;
    let graph = match text(field(m, "graph")?)? {
        "left" => JoinOperand::Left,
        "right" => JoinOperand::Right,
        _ => return Err(fail("C_JOIN", "endpoint graph must be left or right")),
    };
    Ok(JoinEndpoint {
        graph,
        port: text(field(m, "port")?)?.into(),
    })
}
fn id(s: &str) -> Result<Identifier> {
    s.parse().map_err(|e| fail("C_JOIN", e))
}
fn merge<K: Ord + Clone, V: PartialEq + Clone>(
    out: &mut BTreeMap<K, V>,
    other: &BTreeMap<K, V>,
) -> Result<()> {
    for (k, v) in other {
        if out.get(k).is_some_and(|old| old != v) {
            return Err(fail("C_JOIN", "conflicting operand table entries"));
        }
        out.insert(k.clone(), v.clone());
    }
    Ok(())
}
impl Compiler {
    /// Revalidates two self-contained operands and composes only their public ports.
    ///
    /// # Errors
    /// Returns operand admission, join binding/name/conflict or complete composed
    /// graph diagnostics. Failure never returns a partially joined executable.
    pub fn join_graphs(
        &self,
        left: &CanonicalDocument,
        right: &CanonicalDocument,
        join: &JoinSpec,
        catalogs: &MCPCatalogInputs,
    ) -> Result<CompilationSuccess> {
        let l = &self.config.limits;
        let operand_bytes = left
            .encode(l)
            .map_err(|e| fail("C_SERIALIZATION", e))?
            .len()
            .saturating_add(
                right
                    .encode(l)
                    .map_err(|e| fail("C_SERIALIZATION", e))?
                    .len(),
            );
        if operand_bytes > self.config.frontend.max_intermediate_bytes {
            return Err(fail(
                "C_SOURCE_LIMIT",
                "join operands exceed intermediate construction limit",
            ));
        }
        for (label, document) in [("left", left), ("right", right)] {
            self.compile_document(document.clone(), catalogs)
                .map_err(|mut e| {
                    for d in &mut e.diagnostics {
                        d.message = format!("{label} operand: {}", d.message);
                    }
                    e
                })?;
        }
        let work = join
            .edges
            .len()
            .checked_add(join.outputs.len())
            .and_then(|n| {
                join.inputs
                    .iter()
                    .try_fold(n, |n, i| n.checked_add(i.to.len() + 1))
            })
            .ok_or_else(|| fail("C_SOURCE_LIMIT", "join work overflow"))?;
        if work > self.config.frontend.max_work {
            return Err(fail("C_SOURCE_LIMIT", "join work limit exceeded"));
        }
        if join.left_alias == join.right_alias {
            return Err(fail("C_JOIN", "operand aliases must be distinct"));
        }
        let l = &self.config.limits;
        let lf = left.fields();
        let rf = right.fields();
        let roots = [&lf.scopes[&lf.root_scope], &rf.scopes[&rf.root_scope]];
        let index = |side: JoinOperand| match side {
            JoinOperand::Left => 0,
            JoinOperand::Right => 1,
        };
        let aliases = [id(&join.left_alias)?, id(&join.right_alias)?];
        let input = |e: &JoinEndpoint| -> Result<Port> {
            roots[index(e.graph)]
                .fields()
                .inputs
                .get(&e.port)
                .cloned()
                .ok_or_else(|| fail("C_JOIN", "unknown operand public input"))
        };
        let output = |e: &JoinEndpoint| -> Result<Port> {
            roots[index(e.graph)]
                .fields()
                .outputs
                .get(&e.port)
                .cloned()
                .ok_or_else(|| fail("C_JOIN", "unknown operand public output"))
        };
        let source = |e: &JoinEndpoint| -> Result<EdgeSource> {
            Ok(EdgeSource::Output {
                node: aliases[index(e.graph)].clone(),
                port: id(&e.port)?,
            })
        };
        let destination = |e: &JoinEndpoint| -> Result<EdgeDestination> {
            Ok(EdgeDestination::Input {
                node: aliases[index(e.graph)].clone(),
                port: id(&e.port)?,
            })
        };
        let mut bound = BTreeSet::new();
        let mut edge_ids = BTreeSet::new();
        let mut edges = vec![];
        for e in &join.edges {
            if e.from.graph == e.to.graph {
                return Err(fail("C_JOIN", "join edge must connect different operands"));
            }
            output(&e.from)?;
            input(&e.to)?;
            if !bound.insert(e.to.clone()) || !edge_ids.insert(e.id.clone()) {
                return Err(fail("C_JOIN", "duplicate operand input binding or edge ID"));
            }
            edges.push(Edge::new(id(&e.id)?, source(&e.from)?, destination(&e.to)?));
        }
        let mut inputs = join.inputs.iter().collect::<Vec<_>>();
        inputs.sort_by(|a, b| a.name.cmp(&b.name));
        let mut ports = vec![];
        let mut in_names = BTreeSet::new();
        let mut counter = 0;
        for public in inputs {
            if !in_names.insert(&public.name) || public.to.is_empty() {
                return Err(fail("C_JOIN", "duplicate or empty composed input"));
            }
            let mut endpoints = public.to.iter().collect::<Vec<_>>();
            endpoints.sort();
            let mut port: Option<Port> = None;
            for e in endpoints {
                let target = input(e)?;
                if !bound.insert(e.clone()) {
                    return Err(fail(
                        "C_JOIN",
                        "operand input is bound/exposed more than once",
                    ));
                }
                port = Some(if let Some(old) = port {
                    if old.value_type() != target.value_type() {
                        return Err(fail("C_JOIN", "exposed fan-out types differ"));
                    }
                    Port::new(
                        old.value_type().clone(),
                        old.required() || target.required(),
                    )
                } else {
                    target
                });
                let generated = format!("join_in_{counter}");
                counter += 1;
                if !edge_ids.insert(generated.clone()) {
                    return Err(fail("C_JOIN", "generated input edge ID collision"));
                }
                edges.push(Edge::new(
                    id(&generated)?,
                    EdgeSource::Input(id(&public.name)?),
                    destination(e)?,
                ));
            }
            ports.push((id(&public.name)?, port.expect("nonempty destinations")));
        }
        for (side, root) in [
            (JoinOperand::Left, roots[0]),
            (JoinOperand::Right, roots[1]),
        ] {
            for (name, port) in root.fields().inputs.iter() {
                if port.required()
                    && !bound.contains(&JoinEndpoint {
                        graph: side,
                        port: name.to_string(),
                    })
                {
                    return Err(fail(
                        "C_JOIN",
                        "required operand input is neither bound nor exposed",
                    ));
                }
            }
        }
        let inputs = PortTable::new(ports, l).map_err(|e| fail("C_JOIN", e))?;
        let mut outputs = join.outputs.iter().collect::<Vec<_>>();
        outputs.sort_by(|a, b| a.name.cmp(&b.name));
        let mut ports = vec![];
        let mut names = BTreeSet::new();
        for (i, public) in outputs.into_iter().enumerate() {
            if !names.insert(&public.name) {
                return Err(fail("C_JOIN", "duplicate composed output name"));
            }
            ports.push((id(&public.name)?, output(&public.from)?));
            let generated = format!("join_out_{i}");
            if !edge_ids.insert(generated.clone()) {
                return Err(fail("C_JOIN", "generated output edge ID collision"));
            }
            edges.push(Edge::new(
                id(&generated)?,
                source(&public.from)?,
                EdgeDestination::Output(id(&public.name)?),
            ));
        }
        let outputs = PortTable::new(ports, l).map_err(|e| fail("C_JOIN", e))?;
        let mut nodes = vec![];
        for (i, root) in roots.iter().enumerate() {
            let digest = if i == 0 { lf.root_scope } else { rf.root_scope };
            let mut n = NodeFields::new(aliases[i].clone(), Operation::Scope(digest));
            n.inputs = root.fields().inputs.clone();
            n.outputs = root.fields().outputs.clone();
            nodes.push(Node::new(n, l).map_err(|e| fail("C_JOIN", e))?);
        }
        let scope = Scope::new(
            ScopeFields {
                inputs,
                outputs,
                nodes,
                edges,
                ..Default::default()
            },
            l,
        )
        .map_err(|e| fail("C_JOIN", e))?;
        let digest = scope.digest(l).map_err(|e| fail("C_SERIALIZATION", e))?;
        let mut fields = lf.clone();
        merge(&mut fields.scopes, &rf.scopes)?;
        merge(&mut fields.templates, &rf.templates)?;
        merge(&mut fields.bindings, &rf.bindings)?;
        merge(&mut fields.libraries, &rf.libraries)?;
        merge(&mut fields.schema_uris, &rf.schema_uris)?;
        merge(&mut fields.documents, &rf.documents)?;
        fields.graph_id = join.id.clone();
        fields.root_scope = digest;
        fields.scopes.insert(digest, scope);
        let document = CanonicalDocument::new(fields, l).map_err(|e| fail("C_JOIN", e))?;
        self.compile_document(document, catalogs).map_err(|mut e| {
            for d in &mut e.diagnostics {
                d.message = format!("composed result: {}", d.message);
            }
            e
        })
    }
}
