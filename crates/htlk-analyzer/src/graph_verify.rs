//! Scope-local expression admission, binding coverage, and full wait dependencies.
use crate::{
    EdgeDestination, EdgeSource, Expression, ExpressionAnalysis, ExpressionContext as C,
    ExpressionKind as E, ExpressionTypeEnvironment, FunctionId, Identifier, Library, NativeSchemas,
    NodeFields, Operation, Port, PrimitiveType, PromptTemplate, ScalarLiteral, Scope, ScopeContext,
    ScopeFields, TypeContext, ValueReference as R, ValueType, digest::Digest,
};
use htlk_cbor::Limits;
use htlk_executable::cbor as htlk_cbor;
use std::collections::{BTreeMap, BTreeSet};

/// A canonical expression use site within one scope use.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ExpressionSite {
    /// Scope input contract.
    Preconditions,
    /// Scope completion contract.
    Postconditions,
    /// Node admission guard.
    NodeGuard(Identifier),
    /// Node input contract.
    NodePreconditions(Identifier),
    /// Node result contract.
    NodePostconditions(Identifier),
    /// Pure node calculation.
    Eval(Identifier),
    /// Edge selection guard.
    EdgeGuard(Identifier),
    /// Whole-port edge type compatibility (no authored transform expression).
    EdgeBoundary(Identifier),
    /// The containing loop's termination expression for this body use.
    Until,
}
/// Events in the complete intra-scope wait graph, in stable canonical order.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum WaitVertex {
    /// Node guard resolution; precedes active input binding.
    Admission(Identifier),
    /// One node input binding.
    Input {
        /// Local node.
        node: Identifier,
        /// Local input port.
        port: Identifier,
    },
    /// Terminal node outcome and availability of its normal outputs.
    Outcome(Identifier),
    /// One public output binding.
    Output(Identifier),
    /// One loop-next binding.
    Next(Identifier),
    /// Loop termination after body/outgoing boundaries settle.
    Until,
    /// Scope completion, including its result contract.
    Completion,
}
/// One admitted whole-port edge boundary. Absence is preserved for the runtime
/// binding reducer; required node-input absence skips rather than becoming an
/// expression type error.
#[derive(Clone, Debug)]
pub struct EdgeBoundaryPlan {
    edge: Identifier,
    source: Port,
    destination: Port,
    analysis: ExpressionAnalysis,
}
impl EdgeBoundaryPlan {
    /// Canonical edge ID.
    pub fn edge(&self) -> &Identifier {
        &self.edge
    }
    /// Declared producer type and presence.
    pub fn source(&self) -> &Port {
        &self.source
    }
    /// Declared receiving type and presence.
    pub fn destination(&self) -> &Port {
        &self.destination
    }
    /// Value compatibility checks, with absence left to binding reduction.
    pub fn analysis(&self) -> &ExpressionAnalysis {
        &self.analysis
    }
}
/// Candidate group retained for runtime conditional uniqueness checks.
#[derive(Clone, Debug)]
pub struct BindingPlan {
    destination: WaitVertex,
    candidates: Vec<Identifier>,
    required: bool,
}
impl BindingPlan {
    /// Destination binding event.
    pub fn destination(&self) -> &WaitVertex {
        &self.destination
    }
    /// Candidate edges in canonical ID order.
    pub fn candidates(&self) -> &[Identifier] {
        &self.candidates
    }
    /// Whether the destination requires a present value.
    pub fn required(&self) -> bool {
        self.required
    }
    /// More than one selected guard must fail at runtime; no SAT proof is assumed.
    pub fn needs_uniqueness_check(&self) -> bool {
        self.candidates.len() > 1
    }
}
/// Derived local verification data, not an alternative serialized graph.
#[derive(Clone, Debug)]
pub struct ScopeGraphPlan {
    vertices: Vec<WaitVertex>,
    dependencies: Vec<(usize, usize)>,
    topological_order: Vec<usize>,
    expressions: BTreeMap<ExpressionSite, ExpressionAnalysis>,
    environments: BTreeMap<ExpressionSite, ExpressionTypeEnvironment>,
    bindings: Vec<BindingPlan>,
    boundaries: Vec<EdgeBoundaryPlan>,
    bytes: usize,
}
impl ScopeGraphPlan {
    /// Canonically ordered wait events.
    pub fn vertices(&self) -> &[WaitVertex] {
        &self.vertices
    }
    /// Producer/prerequisite to consumer pairs indexing vertices.
    pub fn dependencies(&self) -> &[(usize, usize)] {
        &self.dependencies
    }
    /// Deterministic topological order; lower canonical vertex wins ties.
    pub fn topological_order(&self) -> &[usize] {
        &self.topological_order
    }
    /// Per-use-site expression analysis, including enforceable runtime obligations.
    pub fn expressions(&self) -> &BTreeMap<ExpressionSite, ExpressionAnalysis> {
        &self.expressions
    }
    /// Immutable declarations associated with one analyzed expression site.
    pub fn environment(&self, site: &ExpressionSite) -> Option<&ExpressionTypeEnvironment> {
        self.environments.get(site)
    }
    /// All destination candidate groups, including empty optional groups.
    pub fn bindings(&self) -> &[BindingPlan] {
        &self.bindings
    }
    /// Whole-port edge compatibility plans.
    pub fn boundaries(&self) -> &[EdgeBoundaryPlan] {
        &self.boundaries
    }
}
/// Redacted scope-verification failure with its canonical use site when available.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScopeVerificationError {
    /// Canonical expression location.
    pub site: Option<ExpressionSite>,
    /// Canonical child-index path when an expression subtree is responsible.
    pub expression_path: Option<Vec<usize>>,
    /// Failure classification and retained cause.
    pub kind: ScopeVerificationErrorKind,
}
/// Semantic or resource failure during local scope admission.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ScopeVerificationErrorKind {
    /// Document representation/linkage failed rechecking under the supplied limits.
    Document(Box<crate::DocumentError>),
    /// Representation/use-role failure.
    Graph(crate::GraphRecordError),
    /// Expression/name/type admission failure.
    Expression(Box<crate::ExpressionTypeError>),
    /// An expression root or port is absent from the owning graph context.
    UnknownReference,
    /// Missing/extra loop termination context.
    UseContext,
    /// A node guard depends on that node's own outcome.
    SelfOutcome,
    /// Required destination lacks even a candidate edge.
    MissingCandidate(WaitVertex),
    /// Multiple literal-true writers target one destination.
    UnconditionalWriters(WaitVertex),
    /// Complete wait graph has an intra-iteration cycle.
    Cycle,
    /// Child does not reach an explicit public/completion/loop observation.
    Unobservable(Identifier),
    /// Derived work/storage exceeds the supplied finite limits.
    Limit,
}
impl std::fmt::Display for ScopeVerificationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "scope verification at {:?} {:?}: {:?}",
            self.site, self.expression_path, self.kind
        )
    }
}
impl std::error::Error for ScopeVerificationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match &self.kind {
            ScopeVerificationErrorKind::Document(e) => Some(e.as_ref()),
            ScopeVerificationErrorKind::Graph(e) => Some(e),
            ScopeVerificationErrorKind::Expression(e) => Some(e.as_ref()),
            _ => None,
        }
    }
}
fn error(kind: ScopeVerificationErrorKind) -> ScopeVerificationError {
    ScopeVerificationError {
        site: None,
        expression_path: None,
        kind,
    }
}
fn vertex_size(vertex: &WaitVertex) -> usize {
    let text = match vertex {
        WaitVertex::Admission(name)
        | WaitVertex::Outcome(name)
        | WaitVertex::Output(name)
        | WaitVertex::Next(name) => name.as_str().len(),
        WaitVertex::Input { node, port } => node.as_str().len().saturating_add(port.as_str().len()),
        WaitVertex::Until | WaitVertex::Completion => 0,
    };
    size_of::<WaitVertex>().saturating_add(text)
}
fn expression_error(e: impl Into<crate::ExpressionTypeError>) -> ScopeVerificationError {
    error(ScopeVerificationErrorKind::Expression(Box::new(e.into())))
}
struct Budget<'a> {
    bytes: usize,
    limits: &'a Limits,
}
impl Budget<'_> {
    fn port(&mut self, port: &Port) -> Result<Port, ScopeVerificationError> {
        let size = port
            .value_type()
            .encode(TypeContext::Value, self.limits)
            .map_err(expression_error)?
            .len();
        self.charge(size.saturating_add(size_of::<Port>()))?;
        Ok(port.clone())
    }
    fn charge(&mut self, bytes: usize) -> Result<(), ScopeVerificationError> {
        // Charge one byte per derived entry in addition to its associated data.
        self.bytes = self
            .bytes
            .checked_add(bytes)
            .and_then(|n| n.checked_add(1))
            .ok_or_else(|| error(ScopeVerificationErrorKind::Limit))?;
        if self.bytes > self.limits.max_document_bytes {
            return Err(error(ScopeVerificationErrorKind::Limit));
        }
        Ok(())
    }
}
struct ScopeChecker<'a> {
    scope: &'a ScopeFields,
    nodes: BTreeMap<&'a Identifier, &'a NodeFields>,
    libraries: &'a BTreeMap<Digest, Library>,
    templates: &'a BTreeMap<Digest, PromptTemplate>,
    schemas: &'a NativeSchemas,
    budget: Budget<'a>,
    expressions: BTreeMap<ExpressionSite, ExpressionAnalysis>,
    environments: BTreeMap<ExpressionSite, ExpressionTypeEnvironment>,
    dependencies: BTreeSet<(WaitVertex, WaitVertex)>,
    current_path: Vec<usize>,
}
impl<'a> ScopeChecker<'a> {
    fn node(&self, name: &Identifier) -> Result<&'a NodeFields, ScopeVerificationError> {
        self.nodes
            .get(name)
            .copied()
            .ok_or_else(|| error(ScopeVerificationErrorKind::UnknownReference))
    }
    fn reference(
        &self,
        reference: &R,
        own: Option<&'a NodeFields>,
    ) -> Result<&'a Port, ScopeVerificationError> {
        let port = match reference {
            R::Input(name) => own
                .map_or(&self.scope.inputs, |node| &node.inputs)
                .get(name.as_str()),
            R::ScopeOutput(name) => own
                .map_or(&self.scope.outputs, |node| &node.outputs)
                .get(name.as_str()),
            R::Carried(name) | R::Next(name) => self.scope.carried.get(name.as_str()),
            R::Output { node, port } => self.node(node)?.outputs.get(port.as_str()),
        };
        port.ok_or_else(|| error(ScopeVerificationErrorKind::UnknownReference))
    }
    fn dependency(
        &mut self,
        from: WaitVertex,
        to: WaitVertex,
    ) -> Result<(), ScopeVerificationError> {
        self.budget
            .charge(vertex_size(&from).saturating_add(vertex_size(&to)))?;
        self.dependencies.insert((from, to));
        Ok(())
    }
    fn analyze(
        &mut self,
        site: ExpressionSite,
        expression: &Expression,
        context: C,
        own: Option<&'a NodeFields>,
        expected: &Port,
    ) -> Result<BTreeSet<WaitVertex>, ScopeVerificationError> {
        let result = self.analyze_inner(&site, expression, context, own, expected);
        result.map_err(|mut error| {
            error.site = Some(site);
            if error.expression_path.is_none() {
                error.expression_path = Some(self.current_path.clone());
            }
            error
        })
    }
    fn analyze_inner(
        &mut self,
        site: &ExpressionSite,
        expression: &Expression,
        context: C,
        own: Option<&'a NodeFields>,
        expected: &Port,
    ) -> Result<BTreeSet<WaitVertex>, ScopeVerificationError> {
        let mut env = ExpressionTypeEnvironment::default();
        let mut dependencies = BTreeSet::new();
        let mut pending = vec![(expression, Vec::new())];
        while let Some((expression, path)) = pending.pop() {
            self.current_path = path;
            self.budget.charge(size_of::<&Expression>())?;
            match expression.kind() {
                E::Ref { source, .. } => {
                    if !env.references.contains_key(source) {
                        let port = self.reference(source, own)?;
                        let port = self.budget.port(port)?;
                        env.references.insert(source.clone(), port);
                    }
                    match source {
                        R::Output { node, .. } => {
                            dependencies.insert(WaitVertex::Outcome(node.clone()));
                        }
                        R::ScopeOutput(name) if own.is_none() => {
                            dependencies.insert(WaitVertex::Output(name.clone()));
                        }
                        R::Next(name) => {
                            dependencies.insert(WaitVertex::Next(name.clone()));
                        }
                        _ => (),
                    }
                }
                E::Status(node) | E::Error(node) => {
                    self.node(node)?;
                    if matches!(site,ExpressionSite::NodeGuard(name) if name == node) {
                        return Err(error(ScopeVerificationErrorKind::SelfOutcome));
                    }
                    env.outcomes.insert(node.clone());
                    dependencies.insert(WaitVertex::Outcome(node.clone()));
                }
                E::Call {
                    function: FunctionId::Library { library, .. },
                    ..
                } => self.library(*library, &mut env)?,
                E::FunctionRef { library, .. } => self.library(*library, &mut env)?,
                E::Render { template, .. } if !env.templates.contains_key(template) => {
                    let value = self
                        .templates
                        .get(template)
                        .ok_or_else(|| error(ScopeVerificationErrorKind::UnknownReference))?;
                    self.budget.charge(
                        value
                            .encode(self.budget.limits)
                            .map_err(expression_error)?
                            .len(),
                    )?;
                    env.templates.insert(*template, value.clone());
                }
                _ => (),
            }
            let children: Vec<&Expression> = match expression.kind() {
                E::Call { arguments, .. } | E::List(arguments) => arguments.iter().collect(),
                E::Render { arguments, .. } => arguments.iter().map(|(_, e)| e).collect(),
                E::Record(fields) => fields.iter().map(|(_, e)| e).collect(),
                E::Get { value, .. } | E::Not(value) => vec![value],
                E::Binary { left, right, .. } => vec![left, right],
                _ => Vec::new(),
            };
            for (index, child) in children.into_iter().enumerate().rev() {
                self.budget.charge(
                    size_of_val(self.current_path.as_slice()).saturating_add(size_of::<usize>()),
                )?;
                let mut path = self.current_path.clone();
                path.push(index);
                pending.push((child, path));
            }
        }
        self.current_path.clear();
        let analysis = crate::check_expression_diagnostic(
            expression,
            context,
            &env,
            Some(expected),
            Some(self.schemas),
            self.budget.limits,
        )
        .map_err(|diagnostic| {
            let mut error = expression_error(diagnostic.error);
            error.expression_path = Some(diagnostic.expression_path);
            error
        })?;
        self.budget.charge(
            analysis
                .stored_size(self.budget.limits)
                .map_err(expression_error)?,
        )?;
        self.expressions.insert(site.clone(), analysis);
        self.environments.insert(site.clone(), env);
        Ok(dependencies)
    }
    fn library(
        &mut self,
        id: Digest,
        env: &mut ExpressionTypeEnvironment,
    ) -> Result<(), ScopeVerificationError> {
        if let std::collections::btree_map::Entry::Vacant(entry) = env.libraries.entry(id) {
            let library = self
                .libraries
                .get(&id)
                .ok_or_else(|| error(ScopeVerificationErrorKind::UnknownReference))?;
            self.budget.charge(
                library
                    .encode(self.budget.limits)
                    .map_err(|e| expression_error(crate::ExpressionTypeError::from(e)))?
                    .len(),
            )?;
            entry.insert(library.clone());
        }
        Ok(())
    }
}

/// Verifies one actual ordinary-scope or loop-body use, including its complete
/// wait graph and observability. Loop bodies require that use's `until` expression;
/// reused bodies must be checked for every distinct termination use. Definition
/// linking, schema/MCP admission, and exact native-profile matching are separate
/// stages composed by the executable verifier.
///
/// # Errors
/// Returns names/types, candidate coverage, unconditional writer conflicts,
/// hidden cycles, unobservable children, use-context failures, or derived limits.
pub fn verify_scope_graph(
    scope: &Scope,
    role: ScopeContext,
    until: Option<&Expression>,
    libraries: &BTreeMap<Digest, Library>,
    templates: &BTreeMap<Digest, PromptTemplate>,
    schemas: &NativeSchemas,
    limits: &Limits,
) -> Result<ScopeGraphPlan, ScopeVerificationError> {
    use ScopeVerificationErrorKind as K;
    scope
        .to_value(role, limits)
        .map_err(|e| error(K::Graph(e)))?;
    crate::context::scope(scope, role).map_err(|e| error(K::Graph(e)))?;
    if (role == ScopeContext::LoopBody) != until.is_some() {
        return Err(error(K::UseContext));
    }
    let fields = scope.fields();
    let mut checker = ScopeChecker {
        scope: fields,
        nodes: fields.nodes.iter().map(|n| (n.id(), n.fields())).collect(),
        libraries,
        templates,
        schemas,
        budget: Budget { bytes: 0, limits },
        expressions: BTreeMap::new(),
        environments: BTreeMap::new(),
        dependencies: BTreeSet::new(),
        current_path: Vec::new(),
    };
    let boolean = Port::new(ValueType::primitive(PrimitiveType::Boolean), true);
    let guard_context = C::Guard {
        loop_body: role == ScopeContext::LoopBody,
    };
    let mut vertices = BTreeSet::from([WaitVertex::Completion]);
    let mut groups: BTreeMap<WaitVertex, (Port, Vec<Identifier>, usize)> = BTreeMap::new();
    let mut observed = BTreeSet::new();
    for (name, port) in fields.outputs.iter() {
        let vertex = WaitVertex::Output(name.clone());
        observed.insert(vertex.clone());
        checker
            .budget
            .charge(vertex_size(&vertex).saturating_mul(3))?;
        groups.insert(vertex.clone(), (checker.budget.port(port)?, Vec::new(), 0));
        vertices.insert(vertex);
    }
    if role == ScopeContext::LoopBody {
        vertices.insert(WaitVertex::Until);
        for (name, port) in fields.carried.iter() {
            let vertex = WaitVertex::Next(name.clone());
            observed.insert(vertex.clone());
            checker
                .budget
                .charge(vertex_size(&vertex).saturating_mul(3))?;
            groups.insert(vertex.clone(), (checker.budget.port(port)?, Vec::new(), 0));
            vertices.insert(vertex);
        }
    }
    checker.analyze(
        ExpressionSite::Preconditions,
        &fields.preconditions,
        C::Preconditions,
        None,
        &boolean,
    )?;
    let post = checker.analyze(
        ExpressionSite::Postconditions,
        &fields.postconditions,
        C::ScopePostconditions,
        None,
        &boolean,
    )?;
    observed.extend(post.iter().cloned());
    for dependency in post {
        checker.dependency(dependency, WaitVertex::Completion)?;
    }
    for node in &fields.nodes {
        let n = node.fields();
        let id = node.id();
        let admission = WaitVertex::Admission(id.clone());
        let outcome = WaitVertex::Outcome(id.clone());
        vertices.insert(admission.clone());
        vertices.insert(outcome.clone());
        checker.dependency(admission.clone(), outcome.clone())?;
        checker.dependency(outcome.clone(), WaitVertex::Completion)?;
        for (name, port) in n.inputs.iter() {
            let vertex = WaitVertex::Input {
                node: id.clone(),
                port: name.clone(),
            };
            vertices.insert(vertex.clone());
            checker
                .budget
                .charge(vertex_size(&vertex).saturating_mul(2))?;
            groups.insert(vertex.clone(), (checker.budget.port(port)?, Vec::new(), 0));
            checker.dependency(admission.clone(), vertex.clone())?;
            checker.dependency(vertex, outcome.clone())?;
        }
        for dependency in checker.analyze(
            ExpressionSite::NodeGuard(id.clone()),
            &n.guard,
            guard_context,
            None,
            &boolean,
        )? {
            checker.dependency(dependency, admission.clone())?;
        }
        checker.analyze(
            ExpressionSite::NodePreconditions(id.clone()),
            &n.preconditions,
            C::Preconditions,
            Some(n),
            &boolean,
        )?;
        let post_context = match n.operation {
            Operation::Scope(_) => C::WrapperPostconditions,
            Operation::Loop { .. } => C::LoopPostconditions,
            _ => C::PrimitivePostconditions,
        };
        checker.analyze(
            ExpressionSite::NodePostconditions(id.clone()),
            &n.postconditions,
            post_context,
            Some(n),
            &boolean,
        )?;
        if let Operation::Eval(expression) = &n.operation {
            checker.analyze(
                ExpressionSite::Eval(id.clone()),
                expression,
                C::Eval,
                Some(n),
                n.outputs
                    .get("value")
                    .ok_or_else(|| error(K::UnknownReference))?,
            )?;
        }
    }
    let mut boundaries = Vec::new();
    for edge in &fields.edges {
        let destination = match edge.destination() {
            EdgeDestination::Input { node, port } => WaitVertex::Input {
                node: node.clone(),
                port: port.clone(),
            },
            EdgeDestination::Output(port) => WaitVertex::Output(port.clone()),
            EdgeDestination::Next(port) => WaitVertex::Next(port.clone()),
        };
        let group = groups
            .get_mut(&destination)
            .ok_or_else(|| error(K::UnknownReference))?;
        checker.budget.charge(edge.id().as_str().len())?;
        group.1.push(edge.id().clone());
        if matches!(
            edge.guard().kind(),
            E::Literal(ScalarLiteral::Boolean(true))
        ) {
            group.2 += 1;
        }
        let source = match edge.source() {
            EdgeSource::Input(port) => R::Input(port.clone()),
            EdgeSource::Output { node, port } => {
                checker.dependency(WaitVertex::Outcome(node.clone()), destination.clone())?;
                R::Output {
                    node: node.clone(),
                    port: port.clone(),
                }
            }
            EdgeSource::Carried(port) => R::Carried(port.clone()),
        };
        let source_port = checker.budget.port(checker.reference(&source, None)?)?;
        let mut env = ExpressionTypeEnvironment::default();
        let temporary = R::Input("source".parse().expect("static identifier"));
        env.references
            .insert(temporary.clone(), checker.budget.port(&source_port)?);
        let expression = Expression::new(
            E::Ref {
                source: temporary,
                path: Vec::new(),
            },
            C::Eval,
            limits,
        )
        .map_err(expression_error)?;
        let destination_copy = checker.budget.port(&group.0)?;
        let destination_value = Port::new(destination_copy.value_type().clone(), false);
        let analysis =
            crate::check_expression(&expression, C::Eval, &env, Some(&destination_value), limits)
                .map_err(|e| {
                let mut e = expression_error(e);
                e.site = Some(ExpressionSite::EdgeBoundary(edge.id().clone()));
                e
            })?;
        checker
            .budget
            .charge(analysis.stored_size(limits).map_err(expression_error)?)?;
        boundaries.push(EdgeBoundaryPlan {
            edge: edge.id().clone(),
            source: source_port,
            destination: checker.budget.port(&group.0)?,
            analysis,
        });
        for dependency in checker.analyze(
            ExpressionSite::EdgeGuard(edge.id().clone()),
            edge.guard(),
            guard_context,
            None,
            &boolean,
        )? {
            checker.dependency(dependency, destination.clone())?;
        }
    }
    let mut bindings = Vec::new();
    for (destination, (port, candidates, unconditional)) in groups {
        if port.required() && candidates.is_empty() {
            return Err(error(K::MissingCandidate(destination)));
        }
        if unconditional > 1 {
            return Err(error(K::UnconditionalWriters(destination)));
        }
        if matches!(destination, WaitVertex::Output(_) | WaitVertex::Next(_)) {
            checker.dependency(
                destination.clone(),
                if until.is_some() {
                    WaitVertex::Until
                } else {
                    WaitVertex::Completion
                },
            )?;
        }
        bindings.push(BindingPlan {
            destination,
            candidates,
            required: port.required(),
        });
    }
    if let Some(until) = until {
        for node in &fields.nodes {
            checker.dependency(WaitVertex::Outcome(node.id().clone()), WaitVertex::Until)?;
        }
        let dependencies =
            checker.analyze(ExpressionSite::Until, until, C::LoopUntil, None, &boolean)?;
        observed.extend(dependencies.iter().cloned());
        for dependency in dependencies {
            checker.dependency(dependency, WaitVertex::Until)?;
        }
        checker.dependency(WaitVertex::Until, WaitVertex::Completion)?;
    }
    let vertices: Vec<_> = vertices.into_iter().collect();
    let indexes: BTreeMap<_, _> = vertices.iter().enumerate().map(|(i, v)| (v, i)).collect();
    let mut forward = vec![Vec::new(); vertices.len()];
    let mut reverse = vec![Vec::new(); vertices.len()];
    let mut degrees = vec![0usize; vertices.len()];
    let mut dependencies = Vec::new();
    for (from, to) in checker.dependencies {
        let from = *indexes
            .get(&from)
            .ok_or_else(|| error(K::UnknownReference))?;
        let to = *indexes.get(&to).ok_or_else(|| error(K::UnknownReference))?;
        forward[from].push(to);
        reverse[to].push(from);
        degrees[to] += 1;
        dependencies.push((from, to));
    }
    let mut ready: BTreeSet<_> = degrees
        .iter()
        .enumerate()
        .filter_map(|(i, n)| (*n == 0).then_some(i))
        .collect();
    let mut topological_order = Vec::new();
    while let Some(index) = ready.pop_first() {
        checker.budget.charge(size_of::<usize>())?;
        topological_order.push(index);
        for &next in &forward[index] {
            checker.budget.charge(0)?;
            degrees[next] -= 1;
            if degrees[next] == 0 {
                ready.insert(next);
            }
        }
    }
    if topological_order.len() != vertices.len() {
        return Err(error(K::Cycle));
    }
    let mut live = BTreeSet::new();
    let mut pending: Vec<_> = observed
        .iter()
        .filter_map(|v| indexes.get(v).copied())
        .collect();
    while let Some(index) = pending.pop() {
        checker.budget.charge(0)?;
        if live.insert(index) {
            pending.extend(&reverse[index]);
        }
    }
    for node in &fields.nodes {
        let index = indexes[&WaitVertex::Outcome(node.id().clone())];
        if !live.contains(&index) {
            return Err(error(K::Unobservable(node.id().clone())));
        }
    }
    Ok(ScopeGraphPlan {
        vertices,
        dependencies,
        topological_order,
        expressions: checker.expressions,
        environments: checker.environments,
        bindings,
        boundaries,
        bytes: checker.budget.bytes,
    })
}

/// A use of a definition, rather than an exclusive role attached to its digest.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ScopeUse {
    /// Root scope use.
    Root,
    /// A task or loop operation in an owning scope definition.
    Node {
        /// Owning scope definition.
        scope: Digest,
        /// Local invoking node.
        node: Identifier,
    },
}
/// Complete derived scope-use plans for an already assembled canonical document.
/// Equivalent uses share immutable plans; loop-until changes require independent
/// admission and observability checks.
pub struct GraphVerification {
    document: std::sync::Arc<crate::CanonicalDocument>,
    plans: BTreeMap<ScopeUse, (Digest, std::sync::Arc<ScopeGraphPlan>)>,
}
impl GraphVerification {
    /// Immutable document whose exact scope uses these plans describe.
    pub fn document(&self) -> &crate::CanonicalDocument {
        &self.document
    }
    /// Canonical use locations and the referenced definition/derived plan.
    pub fn plans(&self) -> impl ExactSizeIterator<Item = (&ScopeUse, Digest, &ScopeGraphPlan)> {
        self.plans
            .iter()
            .map(|(site, (digest, plan))| (site, *digest, plan.as_ref()))
    }
    /// A specific scope-use plan.
    pub fn plan(&self, site: &ScopeUse) -> Option<&ScopeGraphPlan> {
        self.plans.get(site).map(|(_, plan)| plan.as_ref())
    }
}
/// Graph admission failure tied to the actual definition use being checked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphVerificationError {
    /// Scope use that failed; absent for document representation failures.
    pub scope_use: Option<ScopeUse>,
    /// Referenced scope definition, when available.
    pub scope: Option<Digest>,
    /// Underlying local semantic failure.
    pub cause: Box<ScopeVerificationError>,
}
impl std::fmt::Display for GraphVerificationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "graph verification at {:?}: {}",
            self.scope_use, self.cause
        )
    }
}
impl std::error::Error for GraphVerificationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.cause.as_ref())
    }
}
/// Checks all actual root/task/loop-body uses, retaining their derived plans.
/// Checks semantic document linkage, then expressions, binding compatibility,
/// wait cycles and observability, retaining the immutable source document.
///
/// # Errors
/// Returns structured local verification or aggregate derived-resource failures.
pub fn verify_graphs(
    document: &crate::CanonicalDocument,
    schemas: &NativeSchemas,
    limits: &Limits,
) -> Result<GraphVerification, GraphVerificationError> {
    crate::linkage::verify(document, limits).map_err(|e| GraphVerificationError {
        scope_use: None,
        scope: None,
        cause: Box::new(error(ScopeVerificationErrorKind::Document(Box::new(e)))),
    })?;
    verify_linked_graphs(std::sync::Arc::new(document.clone()), schemas, limits)
}

pub(crate) fn verify_linked_graphs(
    document: std::sync::Arc<crate::CanonicalDocument>,
    schemas: &NativeSchemas,
    limits: &Limits,
) -> Result<GraphVerification, GraphVerificationError> {
    document
        .to_value(limits)
        .map_err(|e| GraphVerificationError {
            scope_use: None,
            scope: None,
            cause: Box::new(error(ScopeVerificationErrorKind::Document(Box::new(
                e.into(),
            )))),
        })?;
    let fields = document.fields();
    let mut uses = vec![(ScopeUse::Root, fields.root_scope, None)];
    for (scope, definition) in &fields.scopes {
        for node in &definition.fields().nodes {
            let site = ScopeUse::Node {
                scope: *scope,
                node: node.id().clone(),
            };
            match &node.fields().operation {
                Operation::Scope(target) => uses.push((site, *target, None)),
                Operation::Loop { body, until, .. } => uses.push((site, *body, Some(until))),
                _ => (),
            }
        }
    }
    let mut budget = Budget { bytes: 0, limits };
    let mut cache = BTreeMap::new();
    let mut plans = BTreeMap::new();
    for (site, scope, until) in uses {
        let wrap = |cause| GraphVerificationError {
            scope_use: Some(site.clone()),
            scope: Some(scope),
            cause: Box::new(cause),
        };
        budget
            .charge(
                size_of::<ScopeUse>()
                    + match &site {
                        ScopeUse::Root => 0,
                        ScopeUse::Node { node, .. } => node.as_str().len(),
                    },
            )
            .map_err(wrap)?;
        let until_key = until
            .map(|e| {
                e.encode(C::LoopUntil, limits)
                    .map(|v| crate::digest::hash_bytes(&v))
            })
            .transpose()
            .map_err(|e| wrap(expression_error(e)))?;
        let key = (scope, until_key);
        let plan = if let Some(plan) = cache.get(&key) {
            std::sync::Arc::clone(plan)
        } else {
            let definition = fields
                .scopes
                .get(&scope)
                .ok_or_else(|| wrap(error(ScopeVerificationErrorKind::UnknownReference)))?;
            let plan = verify_scope_graph(
                definition,
                if until.is_some() {
                    ScopeContext::LoopBody
                } else {
                    ScopeContext::Ordinary
                },
                until,
                &fields.libraries,
                &fields.templates,
                schemas,
                limits,
            )
            .map_err(wrap)?;
            budget.charge(plan.bytes).map_err(wrap)?;
            let plan = std::sync::Arc::new(plan);
            cache.insert(key, std::sync::Arc::clone(&plan));
            plan
        };
        plans.insert(site, (scope, plan));
    }
    Ok(GraphVerification { document, plans })
}
