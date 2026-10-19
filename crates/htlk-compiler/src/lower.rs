//! Source resolution and canonical record construction. Shared analysis owns typing.
use crate::executable::{
    cbor::{Limits, Map, Value},
    digest::Digest,
};
use crate::{
    Compiler, DiagnosticLocation, MCPCatalogInputs, SourceMapEntry,
    ast::*,
    diagnostic::{Result, fail},
    executable::*,
};
use std::collections::{BTreeMap, BTreeSet};

const RESERVED: &[&str] = &[
    "inputs",
    "outputs",
    "carried",
    "next",
    "length",
    "present",
    "status",
    "error",
    "render",
    "mcp",
    "predicates",
];
pub(crate) fn identifier(s: &str) -> Result<Identifier> {
    s.parse().map_err(|e| fail("C_NAME", e))
}
fn local_name(s: &str) -> Result<()> {
    identifier(s)?;
    if RESERVED.contains(&s) {
        return Err(fail("C_NAME", "reserved name"));
    }
    Ok(())
}
fn value_name(s: &str) -> Result<()> {
    for part in s.split('.') {
        identifier(part)?;
    }
    local_name(s.split('.').next().unwrap_or(""))
}
fn type_name(s: &str) -> Result<()> {
    let mut parts = s.rsplitn(2, '.');
    let last = parts.next().unwrap_or("");
    if !last.as_bytes().first().is_some_and(u8::is_ascii_uppercase)
        || !last.bytes().all(|c| c.is_ascii_alphanumeric())
    {
        return Err(fail("C_NAME", "invalid type declaration name"));
    }
    if let Some(prefix) = parts.next() {
        value_name(prefix)?;
    }
    if builtin(last).is_some() {
        return Err(fail("C_NAME", "built-in type cannot be redeclared"));
    }
    Ok(())
}
fn builtin(s: &str) -> Option<BuiltinType> {
    Some(match s {
        "string" | "text" => BuiltinType::String,
        "integer" => BuiltinType::Integer,
        "float" => BuiltinType::Float,
        "boolean" => BuiltinType::Boolean,
        "null" => BuiltinType::Null,
        "bytes" => BuiltinType::Bytes,
        "json" => BuiltinType::Json,
        "regex" => BuiltinType::Regex,
        "McpResourceResult" => BuiltinType::McpResourceResult,
        "McpPromptResult" => BuiltinType::McpPromptResult,
        "Error" => BuiltinType::Error,
        _ => return None,
    })
}
fn located(
    mut e: crate::CompilationFailure,
    span: &crate::SourceSpan,
) -> crate::CompilationFailure {
    for d in &mut e.diagnostics {
        if matches!(&d.location,DiagnosticLocation::CanonicalRecordPath(p) if p.is_empty()) {
            d.location = DiagnosticLocation::SourceSpan(span.clone());
        }
    }
    e
}

pub(crate) struct Session<'a> {
    pub compiler: &'a Compiler,
    pub catalogs: &'a MCPCatalogInputs,
    pub modules: Vec<Module>,
    pub imports: Vec<BTreeMap<String, usize>>,
    pub symbols: Vec<BTreeMap<String, Decl>>,
    types: BTreeMap<(usize, String), ValueType>,
    prompts: BTreeMap<(usize, String), Digest>,
    tasks: BTreeMap<(usize, String), Digest>,
    active: BTreeSet<(usize, String)>,
    pub pool: DocumentFields,
    pub source_map: Vec<SourceMapEntry>,
    pub documentation: BTreeMap<String, String>,
    pub diagnostics: Vec<crate::Diagnostic>,
    work: usize,
    depth: usize,
    intermediate_bytes: usize,
    diagnostic_bytes: usize,
}
impl<'a> Session<'a> {
    pub fn new(
        compiler: &'a Compiler,
        catalogs: &'a MCPCatalogInputs,
        modules: Vec<Module>,
        imports: Vec<BTreeMap<String, usize>>,
    ) -> Result<Self> {
        catalogs.check_limits(&compiler.config.limits)?;
        let mut symbols = vec![];
        for (i, m) in modules.iter().enumerate() {
            let mut names = BTreeMap::new();
            for d in &m.declarations {
                let checked = match &d.kind {
                    DeclKind::Type(_) => type_name(&d.name),
                    DeclKind::Library(..) => local_name(&d.name),
                    _ => value_name(&d.name),
                };
                checked.map_err(|e| located(e, &d.span))?;
                if let Some(previous) = names.insert(d.name.clone(), d.clone()) {
                    return Err(located(
                        crate::diagnostic::related(
                            "C_DUPLICATE",
                            "duplicate declaration",
                            &previous.span,
                        ),
                        &d.span,
                    ));
                }
            }
            for alias in imports[i].keys() {
                local_name(alias)?;
                if names
                    .keys()
                    .any(|n| n.split('.').next() == Some(alias.as_str()))
                {
                    return Err(fail(
                        "C_IMPORT",
                        "import shadows local declaration namespace",
                    ));
                }
            }
            symbols.push(names);
        }
        let mut pool = DocumentFields::new(
            "compiler.validation".into(),
            compiler.config.profile.clone(),
            Digest::from_bytes([0; 32]),
        );
        pool.documents.insert(
            compiler.config.policy.digest(),
            compiler.config.policy.document().clone(),
        );
        Ok(Self {
            compiler,
            catalogs,
            modules,
            imports,
            symbols,
            types: BTreeMap::new(),
            prompts: BTreeMap::new(),
            tasks: BTreeMap::new(),
            active: BTreeSet::new(),
            pool,
            source_map: vec![],
            documentation: BTreeMap::new(),
            diagnostics: Vec::new(),
            work: 0,
            depth: 0,
            intermediate_bytes: 0,
            diagnostic_bytes: 0,
        })
    }
    fn l(&self) -> &Limits {
        &self.compiler.config.limits
    }
    fn charge(&mut self) -> Result<()> {
        self.charge_amount(1)
    }
    fn charge_amount(&mut self, amount: usize) -> Result<()> {
        self.work = self
            .work
            .checked_add(amount)
            .ok_or_else(|| fail("C_SOURCE_LIMIT", "work overflow"))?;
        if self.work > self.compiler.config.frontend.max_work {
            Err(fail("C_SOURCE_LIMIT", "frontend work limit exceeded"))
        } else {
            Ok(())
        }
    }
    fn account(&mut self, bytes: usize) -> Result<()> {
        self.intermediate_bytes = self
            .intermediate_bytes
            .checked_add(bytes)
            .ok_or_else(|| fail("C_SOURCE_LIMIT", "intermediate size overflow"))?;
        if self.intermediate_bytes > self.compiler.config.frontend.max_intermediate_bytes {
            return Err(fail(
                "C_SOURCE_LIMIT",
                "intermediate construction byte limit exceeded",
            ));
        }
        Ok(())
    }
    fn report_limit(&mut self) -> Result<()> {
        self.charge_amount(
            self.source_map
                .len()
                .saturating_add(self.documentation.len()),
        )?;
        let bytes = self.diagnostic_bytes.saturating_add(
            self.source_map
                .iter()
                .fold(0usize, |n, e| {
                    n.saturating_add(crate::diagnostic::map_size(e))
                })
                .saturating_add(self.documentation.iter().fold(0usize, |n, (k, v)| {
                    n.saturating_add(k.len()).saturating_add(v.len())
                })),
        );
        if bytes > self.compiler.config.frontend.max_report_bytes {
            return Err(fail(
                "C_SOURCE_LIMIT",
                "compiler provenance/report limit exceeded",
            ));
        }
        Ok(())
    }
    fn enter(&mut self) -> Result<()> {
        self.charge()?;
        self.depth += 1;
        if self.depth > self.compiler.config.frontend.max_depth {
            Err(fail("C_SOURCE_LIMIT", "resolution nesting limit exceeded"))
        } else {
            Ok(())
        }
    }
    fn resolve(&self, module: usize, name: &str) -> Result<(usize, Decl)> {
        if let Some((alias, rest)) = name.split_once('.')
            && let Some(target) = self.imports[module].get(alias)
        {
            let d = self.symbols[*target]
                .get(rest)
                .ok_or_else(|| fail("C_NAME", format!("unknown imported declaration {name}")))?;
            if !d.exported {
                return Err(crate::diagnostic::related(
                    "C_VISIBILITY",
                    &format!("private imported declaration {name}"),
                    &d.span,
                ));
            }
            return Ok((*target, d.clone()));
        }
        self.symbols[module]
            .get(name)
            .cloned()
            .map(|d| (module, d))
            .ok_or_else(|| fail("C_NAME", format!("unknown declaration {name}")))
    }
    pub fn check_all(&mut self) -> Result<()> {
        for module in 0..self.modules.len() {
            let declarations = self.modules[module].declarations.clone();
            for d in declarations {
                self.charge()?;
                let result = (|| match &d.kind {
                    DeclKind::Type(t) => self.ty(module, t).map(|_| ()),
                    DeclKind::Prompt(..) => self.prompt(module, &d.name).map(|_| ()),
                    DeclKind::Library(..) => self.library(module, &d.name).map(|_| ()),
                    DeclKind::Task(_) | DeclKind::Graph(_) => {
                        let id = self.scope_decl(module, &d.name)?;
                        let doc = self.document("compiler.validation", id)?;
                        let out = self.compiler.compile_document(doc, self.catalogs)?;
                        for diagnostic in out.diagnostics {
                            self.charge_amount(self.diagnostics.len().saturating_add(1))?;
                            if self.diagnostics.contains(&diagnostic) {
                                continue;
                            }
                            self.diagnostic_bytes = self
                                .diagnostic_bytes
                                .saturating_add(crate::diagnostic::diagnostic_size(&diagnostic));
                            self.report_limit()?;
                            self.diagnostics.push(diagnostic);
                        }
                        Ok(())
                    }
                })();
                result.map_err(|e| self.source_error(located(e, &d.span)))?;
                self.report_limit()?;
            }
        }
        Ok(())
    }
    pub fn ty(&mut self, module: usize, t: &Ty) -> Result<ValueType> {
        self.enter()?;
        let result = match t {
            Ty::Name(n) => {
                if let Some(b) = builtin(n) {
                    Ok(ValueType::builtin(b))
                } else {
                    let (m, d) = self.resolve(module, n)?;
                    let DeclKind::Type(t) = d.kind else {
                        return Err(crate::diagnostic::related(
                            "C_NAME",
                            "expected type declaration",
                            &d.span,
                        ));
                    };
                    let key = (m, d.name.clone());
                    if let Some(t) = self.types.get(&key) {
                        Ok(t.clone())
                    } else {
                        if !self.active.insert(key.clone()) {
                            return Err(located(fail("C_TYPE", "type alias cycle"), &d.span));
                        }
                        let t = self.ty(m, &t)?;
                        self.active.remove(&key);
                        self.types.insert(key, t.clone());
                        Ok(t)
                    }
                }
            }
            Ty::List(t) => {
                let t = self.ty(module, t)?;
                self.make_type(ValueTypeKind::List(Box::new(t)))
            }
            Ty::Map(t) => {
                let t = self.ty(module, t)?;
                self.make_type(ValueTypeKind::Map(Box::new(t)))
            }
            Ty::Union(ts) => {
                let mut out = vec![];
                for t in ts {
                    out.push(self.ty(module, t)?);
                }
                self.make_type(ValueTypeKind::Union(out))
            }
            Ty::Enum(values) => self.make_type(ValueTypeKind::Enum(values.clone())),
            Ty::Record(fields) => {
                let mut out = vec![];
                for (n, p) in fields {
                    out.push((n.clone(), self.port(module, p)?));
                }
                self.make_type(ValueTypeKind::Record(out))
            }
        };
        self.depth -= 1;
        if let Ok(t) = &result {
            self.account(
                t.encode(TypeContext::Value, self.l())
                    .map_err(type_error)?
                    .len(),
            )?;
        }
        result
    }
    fn make_type(&self, k: ValueTypeKind) -> Result<ValueType> {
        ValueType::new(k, TypeContext::Value, self.l()).map_err(type_error)
    }
    fn port(&mut self, m: usize, p: &PortTy) -> Result<Port> {
        Ok(Port::new(self.ty(m, &p.ty)?, p.required))
    }
    fn ports(&mut self, m: usize, ports: Option<&Ports>) -> Result<PortTable> {
        let mut out = vec![];
        for (n, p) in ports.into_iter().flatten() {
            out.push((identifier(n)?, self.port(m, p)?));
        }
        PortTable::new(out, self.l()).map_err(graph_error)
    }
    fn library(&mut self, module: usize, alias: &str) -> Result<Library> {
        let (_, d) = self.resolve(module, alias)?;
        let DeclKind::Library(id, version, pin) = d.kind else {
            return Err(fail("C_NAME", "expected library alias"));
        };
        let digest: Digest = pin.parse().map_err(|e| fail("C_LIBRARY", e))?;
        let library = self
            .compiler
            .config
            .libraries
            .get(&digest)
            .filter(|l| l.library_id() == id && l.version() == version)
            .ok_or_else(|| {
                fail(
                    "C_LIBRARY",
                    "library selection does not match linked registry",
                )
            })?
            .clone();
        self.pool.libraries.insert(digest, library.clone());
        Ok(library)
    }
    pub fn prompt(&mut self, module: usize, name: &str) -> Result<Digest> {
        let (m, d) = self.resolve(module, name)?;
        let key = (m, d.name.clone());
        if let Some(id) = self.prompts.get(&key) {
            return Ok(*id);
        }
        let DeclKind::Prompt(parameters, text) = d.kind else {
            return Err(fail("C_NAME", "expected prompt declaration"));
        };
        let mut parts = vec![];
        let mut literal = String::new();
        let mut slots = BTreeSet::new();
        let mut chars = text.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '{' => {
                    if chars.peek() == Some(&'{') {
                        chars.next();
                        literal.push('{');
                        continue;
                    }
                    parts.push(TemplatePart::Text(std::mem::take(&mut literal)));
                    let mut name = String::new();
                    loop {
                        match chars.next() {
                            Some('}') => break,
                            Some(c) => name.push(c),
                            None => return Err(fail("C_PROMPT", "unterminated prompt slot")),
                        }
                    }
                    let id =
                        identifier(&name).map_err(|_| fail("C_PROMPT", "invalid prompt slot"))?;
                    slots.insert(name);
                    parts.push(TemplatePart::Slot(id));
                }
                '}' => {
                    if chars.next() != Some('}') {
                        return Err(fail("C_PROMPT", "unescaped closing prompt brace"));
                    }
                    literal.push('}');
                }
                _ => literal.push(c),
            }
        }
        parts.push(TemplatePart::Text(literal));
        let mut params = vec![];
        if let Some(parameters) = parameters {
            for (n, t) in parameters {
                params.push((identifier(&n)?, Port::new(self.ty(m, &t)?, true)));
            }
        } else {
            for n in slots {
                params.push((
                    identifier(&n)?,
                    Port::new(ValueType::builtin(BuiltinType::String), true),
                ));
            }
        }
        let template = PromptTemplate::new(params, parts, self.l()).map_err(expression_error)?;
        crate::analyzer::check_prompt_template(&template, self.l())
            .map_err(|e| fail("C_PROMPT", e))?;
        let id = template.digest(self.l()).map_err(|e| fail("C_PROMPT", e))?;
        self.pool.templates.insert(id, template);
        self.prompts.insert(key, id);
        self.source_map.push(SourceMapEntry {
            canonical_path: format!("/templates/{id}"),
            span: d.span,
            declaration_name: Some(d.name),
        });
        self.report_limit()?;
        Ok(id)
    }
    fn function(&mut self, module: usize, name: &[String]) -> Result<FunctionId> {
        match name {
            [name] if name == "length" => Ok(FunctionId::Core(CoreFunction::Length)),
            [name] if name == "present" => Ok(FunctionId::Core(CoreFunction::Present)),
            [alias, name] => {
                let library = self.library(module, alias)?;
                if library.function(name).is_none() {
                    return Err(fail("C_NAME", "unknown linked library function"));
                }
                Ok(FunctionId::Library {
                    library: library.implementation_digest(),
                    name: identifier(name)?,
                })
            }
            _ => Err(fail("C_NAME", "invalid pure function reference")),
        }
    }
    fn expr(&mut self, m: usize, e: &Expr) -> Result<Expression> {
        self.enter()?;
        let result = self
            .expr_inner(m, e)
            .map_err(|error| located(error, &e.span));
        self.depth -= 1;
        if let Ok(e) = &result {
            self.account(e.encode(self.l()).map_err(expression_error)?.len())?;
        }
        result
    }
    fn expr_inner(&mut self, m: usize, e: &Expr) -> Result<Expression> {
        let k = match &e.value {
            ExprKind::Literal(v) => ExpressionKind::Literal(v.clone()),
            ExprKind::Regex(p, f) => ExpressionKind::Regex {
                pattern: p.clone(),
                flags: f.clone(),
            },
            ExprKind::Name(parts) => {
                let (source, rest) = match parts.as_slice() {
                    [root, port, rest @ ..] if root == "inputs" => {
                        (ValueReference::Input(identifier(port)?), rest)
                    }
                    [root, port, rest @ ..] if root == "outputs" => {
                        (ValueReference::ScopeOutput(identifier(port)?), rest)
                    }
                    [root, port, rest @ ..] if root == "carried" => {
                        (ValueReference::Carried(identifier(port)?), rest)
                    }
                    [root, port, rest @ ..] if root == "next" => {
                        (ValueReference::Next(identifier(port)?), rest)
                    }
                    [node, root, port, rest @ ..] if root == "outputs" => (
                        ValueReference::Output {
                            node: identifier(node)?,
                            port: identifier(port)?,
                        },
                        rest,
                    ),
                    _ => return Err(fail("C_NAME", "invalid value reference")),
                };
                ExpressionKind::Ref {
                    source,
                    path: rest.iter().map(|s| PathStep::Field(s.clone())).collect(),
                }
            }
            ExprKind::Get(value, step) => ExpressionKind::Get {
                value: Box::new(self.expr(m, value)?),
                path: vec![step.clone()],
            },
            ExprKind::List(values) => {
                let mut out = vec![];
                for e in values {
                    out.push(self.expr(m, e)?);
                }
                ExpressionKind::List(out)
            }
            ExprKind::Record(values) => {
                let mut out = vec![];
                for (n, e) in values {
                    out.push((n.clone(), self.expr(m, e)?));
                }
                ExpressionKind::Record(out)
            }
            ExprKind::Not(e) => ExpressionKind::Not(Box::new(self.expr(m, e)?)),
            ExprKind::Binary(op, a, b) => ExpressionKind::Binary {
                operator: *op,
                left: Box::new(self.expr(m, a)?),
                right: Box::new(self.expr(m, b)?),
            },
            ExprKind::Static(name) => {
                let FunctionId::Library { library, name } = self.function(m, name)? else {
                    return Err(fail(
                        "C_NAME",
                        "core function reference is not a library callback",
                    ));
                };
                ExpressionKind::FunctionRef { library, name }
            }
            ExprKind::Handle(_) => {
                return Err(fail(
                    "C_NAME",
                    "node handles are legal only in status/error",
                ));
            }
            ExprKind::Call(name, args) => {
                if name.len() == 1 && matches!(name[0].as_str(), "status" | "error") {
                    let [
                        Expr {
                            value: ExprKind::Handle(node),
                            ..
                        },
                    ] = args.as_slice()
                    else {
                        return Err(fail("C_NAME", "status/error require one node handle"));
                    };
                    if name[0] == "status" {
                        ExpressionKind::Status(identifier(node)?)
                    } else {
                        ExpressionKind::Error(identifier(node)?)
                    }
                } else if name.as_slice() == ["render"] {
                    let [
                        Expr {
                            value: ExprKind::Static(name),
                            ..
                        },
                        Expr {
                            value: ExprKind::Record(args),
                            ..
                        },
                    ] = args.as_slice()
                    else {
                        return Err(fail(
                            "C_PROMPT",
                            "render requires static template and argument record",
                        ));
                    };
                    let template = self.prompt(m, &name.join("."))?;
                    let mut arguments = vec![];
                    for (n, e) in args {
                        arguments.push((identifier(n)?, self.expr(m, e)?));
                    }
                    ExpressionKind::Render {
                        template,
                        arguments,
                    }
                } else {
                    let function = self.function(m, name)?;
                    let mut arguments = vec![];
                    for e in args {
                        arguments.push(self.expr(m, e)?);
                    }
                    ExpressionKind::Call {
                        function,
                        arguments,
                    }
                }
            }
        };
        Expression::new(k, self.l()).map_err(expression_error)
    }
    fn condition(&mut self, m: usize, e: Option<&Expr>) -> Result<Expression> {
        match e {
            Some(e) => self.expr(m, e),
            None => Ok(Expression::literal(ScalarLiteral::Boolean(true))),
        }
    }
    fn limits(&self, b: &Block) -> Result<ExecutionLimits> {
        let values = b
            .limits
            .iter()
            .flat_map(|m| m.iter())
            .map(|(n, v)| (n.clone(), Value::Integer(*v)));
        let value = Value::Map(Map::try_from_entries(values).map_err(|e| fail("C_POLICY", e))?);
        ExecutionLimits::from_value(&value, self.l()).map_err(|e| fail("C_POLICY", e))
    }
    pub fn scope_decl(&mut self, module: usize, name: &str) -> Result<Digest> {
        self.enter()?;
        let result = self.scope_decl_inner(module, name);
        self.depth -= 1;
        result
    }
    fn scope_decl_inner(&mut self, module: usize, name: &str) -> Result<Digest> {
        let (m, d) = self.resolve(module, name)?;
        let key = (m, d.name.clone());
        if let Some(id) = self.tasks.get(&key) {
            return Ok(*id);
        }
        if !self.active.insert(key.clone()) {
            return Err(located(
                fail("C_DEPENDENCY_CYCLE", "task containment cycle"),
                &d.span,
            ));
        }
        let block = match &d.kind {
            DeclKind::Task(b) | DeclKind::Graph(b) => b,
            _ => return Err(fail("C_NAME", "expected task declaration")),
        };
        let id = self.scope(m, block, None)?;
        self.active.remove(&key);
        self.tasks.insert(key, id);
        let path = format!("/scopes/{id}");
        self.source_map.push(SourceMapEntry {
            canonical_path: path.clone(),
            span: d.span.clone(),
            declaration_name: Some(d.name),
        });
        if let Some(description) = &block.description {
            self.documentation.insert(path, description.clone());
        }
        Ok(id)
    }
    fn scope(
        &mut self,
        m: usize,
        b: &Block,
        interfaces: Option<(PortTable, PortTable, PortTable)>,
    ) -> Result<Digest> {
        self.enter()?;
        let result = self.scope_inner(m, b, interfaces);
        self.depth -= 1;
        result
    }
    fn scope_inner(
        &mut self,
        m: usize,
        b: &Block,
        interfaces: Option<(PortTable, PortTable, PortTable)>,
    ) -> Result<Digest> {
        let (inputs, outputs, carried) = match interfaces {
            Some(i) => i,
            None => (
                self.ports(m, b.inputs.as_ref())?,
                self.ports(m, b.outputs.as_ref())?,
                PortTable::default(),
            ),
        };
        let mut nodes = vec![];
        for n in b.nodes.iter().flatten() {
            nodes.push(self.node(m, n).map_err(|e| located(e, &n.span))?);
        }
        let mut edges = vec![];
        for e in b.edges.iter().flatten() {
            let from = match e
                .value
                .from
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
                .as_slice()
            {
                ["inputs", port] => EdgeSource::Input(identifier(port)?),
                ["carried", port] => EdgeSource::Carried(identifier(port)?),
                [node, "outputs", port] => EdgeSource::Output {
                    node: identifier(node)?,
                    port: identifier(port)?,
                },
                _ => return Err(located(fail("C_BINDING", "invalid edge source"), &e.span)),
            };
            let to = match e
                .value
                .to
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
                .as_slice()
            {
                ["outputs", port] => EdgeDestination::Output(identifier(port)?),
                ["next", port] => EdgeDestination::Next(identifier(port)?),
                [node, "inputs", port] => EdgeDestination::Input {
                    node: identifier(node)?,
                    port: identifier(port)?,
                },
                _ => {
                    return Err(located(
                        fail("C_BINDING", "invalid edge destination"),
                        &e.span,
                    ));
                }
            };
            edges.push(
                Edge::new(identifier(&e.value.id)?, from, to)
                    .with_guard(self.condition(m, e.value.guard.as_ref())?),
            );
        }
        let scope = Scope::new(
            ScopeFields {
                inputs,
                outputs,
                carried,
                nodes,
                edges,
                preconditions: self.condition(m, b.preconditions.as_ref())?,
                postconditions: self.condition(m, b.postconditions.as_ref())?,
                limits: self.limits(b)?,
            },
            self.l(),
        )
        .map_err(graph_error)?;
        let id = scope
            .digest(self.l())
            .map_err(|e| fail("C_SERIALIZATION", e))?;
        for (i, n) in scope.fields().nodes.iter().enumerate() {
            let source = b
                .nodes
                .as_ref()
                .and_then(|ns| ns.iter().find(|s| s.value.id == n.id().as_str()))
                .expect("lowered source node");
            let path = format!("/scopes/{id}/nodes/{i}");
            self.source_map.push(SourceMapEntry {
                canonical_path: path.clone(),
                span: source.span.clone(),
                declaration_name: None,
            });
            if let Some(d) = &source.value.options.description {
                self.documentation.insert(path.clone(), d.clone());
            }
            self.expression_sources(&path, n.fields(), &source.value);
        }
        for (i, e) in scope.fields().edges.iter().enumerate() {
            let source = b
                .edges
                .as_ref()
                .and_then(|es| es.iter().find(|s| s.value.id == e.id().as_str()))
                .expect("lowered source edge");
            let path = format!("/scopes/{id}/edges/{i}");
            self.source_map.push(SourceMapEntry {
                canonical_path: path.clone(),
                span: source.span.clone(),
                declaration_name: None,
            });
            if let Some(d) = &source.value.description {
                self.documentation.insert(path.clone(), d.clone());
            }
            if let Some(authored) = &source.value.guard {
                crate::provenance::expression(
                    &format!("{path}/guard"),
                    e.guard(),
                    authored,
                    &mut self.source_map,
                );
            }
        }
        for (name, e) in [
            ("preconditions", b.preconditions.as_ref()),
            ("postconditions", b.postconditions.as_ref()),
        ] {
            if let Some(e) = e {
                let normalized = if name == "preconditions" {
                    &scope.fields().preconditions
                } else {
                    &scope.fields().postconditions
                };
                crate::provenance::expression(
                    &format!("/scopes/{id}/{name}"),
                    normalized,
                    e,
                    &mut self.source_map,
                );
            }
        }
        self.account(scope.encode(self.l()).map_err(graph_error)?.len())?;
        self.pool.scopes.insert(id, scope);
        self.report_limit()?;
        Ok(id)
    }
    fn expression_sources(&mut self, path: &str, normalized: &NodeFields, n: &AstNode) {
        for (suffix, value, e) in [
            ("guard", &normalized.guard, n.options.guard.as_ref()),
            (
                "preconditions",
                &normalized.preconditions,
                n.options.preconditions.as_ref(),
            ),
            (
                "postconditions",
                &normalized.postconditions,
                n.options.postconditions.as_ref(),
            ),
        ] {
            if let Some(e) = e {
                crate::provenance::expression(
                    &format!("{path}/{suffix}"),
                    value,
                    e,
                    &mut self.source_map,
                );
            }
        }
        if let (AstOperation::Eval(_, source), Operation::Eval(value)) =
            (&n.operation, &normalized.operation)
        {
            crate::provenance::expression(
                &format!("{path}/operation/1"),
                value,
                source,
                &mut self.source_map,
            );
        }
        if let (Some(source), Operation::Loop { until, .. }) =
            (&n.options.until, &normalized.operation)
        {
            crate::provenance::expression(
                &format!("{path}/operation/3"),
                until,
                source,
                &mut self.source_map,
            );
        }
    }
    fn node(&mut self, m: usize, n: &Located<AstNode>) -> Result<Node> {
        let n = &n.value;
        local_name(&n.id)?;
        let b = &n.options;
        if b.inputs.is_some() && !matches!(n.operation, AstOperation::Eval(..) | AstOperation::Loop)
        {
            return Err(fail("C_SCOPE", "inputs option is legal only on eval"));
        }
        if (b.pin.is_some() || b.retry.is_some()) && !matches!(n.operation, AstOperation::Mcp(..)) {
            return Err(fail("C_MCP", "pin/retry require MCP operation"));
        }
        if (b.topic.is_some() || b.timeout_ms.is_some())
            && !matches!(n.operation, AstOperation::Wait(_))
        {
            return Err(fail("C_SCOPE", "topic/timeout_ms require wait"));
        }
        let (operation, inputs, outputs) = match &n.operation {
            AstOperation::Eval(t, e) => (
                Operation::Eval(self.expr(m, e)?),
                self.ports(m, b.inputs.as_ref())?,
                PortTable::new(vec![(identifier("value")?, self.port(m, t)?)], self.l())
                    .map_err(|e| fail("C_TYPE", e))?,
            ),
            AstOperation::Wait(t) => (
                Operation::Wait {
                    topic: b
                        .topic
                        .clone()
                        .ok_or_else(|| fail("C_SCOPE", "missing wait topic"))?,
                    timeout_ms: b
                        .timeout_ms
                        .ok_or_else(|| fail("C_SCOPE", "missing wait timeout_ms"))?
                        .try_into()
                        .map_err(|_| fail("C_POLICY", "negative wait deadline"))?,
                },
                one_port("request", ValueType::builtin(BuiltinType::Json), self.l())?,
                one_port("value", self.ty(m, t)?, self.l())?,
            ),
            AstOperation::Use(name) => {
                let (_, d) = self.resolve(m, name)?;
                if !matches!(d.kind, DeclKind::Task(_)) {
                    return Err(crate::diagnostic::related(
                        "C_NAME",
                        "use requires a task",
                        &d.span,
                    ));
                }
                let id = self.scope_decl(m, name)?;
                let f = self.pool.scopes[&id].fields();
                (Operation::Scope(id), f.inputs.clone(), f.outputs.clone())
            }
            AstOperation::Loop => {
                let inputs = self.ports(m, b.inputs.as_ref())?;
                let outputs = self.ports(m, b.outputs.as_ref())?;
                let mut carried = vec![];
                let mut initializers = vec![];
                for (name, input) in b.carried.iter().flatten() {
                    let p = inputs
                        .get(input)
                        .ok_or_else(|| fail("C_LOOP", "unknown carried initializer input"))?;
                    carried.push((identifier(name)?, p.clone()));
                    initializers.push((identifier(name)?, identifier(input)?));
                }
                let carried = PortTable::new(carried, self.l()).map_err(|e| fail("C_LOOP", e))?;
                let body = self.scope(
                    m,
                    b.body
                        .as_deref()
                        .ok_or_else(|| fail("C_LOOP", "missing body"))?,
                    Some((inputs.clone(), outputs.clone(), carried)),
                )?;
                (
                    Operation::Loop {
                        body,
                        initializers,
                        until: self.expr(
                            m,
                            b.until
                                .as_ref()
                                .ok_or_else(|| fail("C_LOOP", "missing until"))?,
                        )?,
                        max_iterations: b
                            .max_iterations
                            .ok_or_else(|| fail("C_LOOP", "missing max_iterations"))?
                            .try_into()
                            .map_err(|_| fail("C_LOOP", "negative max_iterations"))?,
                    },
                    inputs,
                    outputs,
                )
            }
            AstOperation::Mcp(category, server, selection) => {
                let (binding, inputs, outputs) =
                    self.mcp(category, server, selection, b.pin.as_deref())?;
                let retry = if let Some(retry) = &b.retry {
                    let count = literal_value(&retry["max_attempts"])?;
                    let on = literal_value(&retry["on"])?;
                    let backoff = literal_value(&retry["backoff_ms"])?;
                    let number = |v: &Value| -> Result<u64> {
                        match v {
                            Value::Integer(n) => u64::try_from(*n)
                                .map_err(|_| fail("C_MCP", "negative retry count/delay")),
                            _ => Err(fail("C_MCP", "retry count/delays must be integers")),
                        }
                    };
                    let codes = crate::input_value::array(&on)?
                        .iter()
                        .map(|v| crate::input_value::text(v).map(str::to_owned))
                        .collect::<Result<Vec<_>>>()?;
                    let delays = crate::input_value::array(&backoff)?
                        .iter()
                        .map(number)
                        .collect::<Result<Vec<_>>>()?;
                    RetryPolicy::new(number(&count)?, codes, delays, self.l())
                        .map_err(|e| fail("C_MCP", e))?
                } else {
                    RetryPolicy::no_retry()
                };
                (Operation::Mcp { binding, retry }, inputs, outputs)
            }
        };
        let mut f = NodeFields::new(identifier(&n.id)?, operation);
        f.inputs = inputs;
        f.outputs = outputs;
        f.guard = self.condition(m, b.guard.as_ref())?;
        f.preconditions = self.condition(m, b.preconditions.as_ref())?;
        f.postconditions = self.condition(m, b.postconditions.as_ref())?;
        f.limits = self.limits(b)?;
        Node::new(f, self.l()).map_err(graph_error)
    }
    fn mcp(
        &mut self,
        category: &str,
        alias: &str,
        selection: &str,
        pin: Option<&str>,
    ) -> Result<(Digest, PortTable, PortTable)> {
        use crate::input_value::{array, field, map, text};
        let server = self
            .catalogs
            .servers
            .get(alias)
            .ok_or_else(|| fail("C_MCP", "unknown MCP server alias"))?
            .clone();
        let category = match category {
            "tool" => "tools",
            "resource" => "resources",
            "template" => "templates",
            _ => "prompts",
        };
        let descriptor = self
            .catalogs
            .entries
            .get(&(
                server.digest(self.l()).map_err(|e| fail("C_MCP", e))?,
                category.into(),
                selection.into(),
            ))
            .ok_or_else(|| fail("C_MCP", "unknown MCP selection"))?
            .clone();
        if let Some(pin) = pin {
            let pin: Digest = pin.parse().map_err(|e| fail("C_MCP", e))?;
            if pin != descriptor.digest() {
                return Err(fail("C_MCP", "source pin mismatch"));
            }
        }
        let (kind, inputs, output) = match category {
            "tools" => {
                let d = map(descriptor.value())?;
                let input = JsonDocument::from_value(field(d, "inputSchema")?, self.l())
                    .map_err(|e| fail("C_SCHEMA", e))?;
                let output = JsonDocument::from_value(
                    field(d, "outputSchema")
                        .map_err(|_| fail("C_MCP", "selected tool requires outputSchema"))?,
                    self.l(),
                )
                .map_err(|e| fail("C_SCHEMA", e))?;
                let input_id = input.digest();
                let output_id = output.digest();
                self.pool.documents.insert(input_id, input);
                self.pool.documents.insert(output_id, output);
                (
                    McpBindingKind::Tool {
                        name: selection.into(),
                        input_schema: input_id,
                        output_schema: output_id,
                    },
                    one_port(
                        "arguments",
                        self.make_type(ValueTypeKind::Schema(input_id))?,
                        self.l(),
                    )?,
                    self.make_type(ValueTypeKind::Schema(output_id))?,
                )
            }
            "resources" => (
                McpBindingKind::Resource {
                    uri: selection.into(),
                },
                PortTable::default(),
                ValueType::builtin(BuiltinType::McpResourceResult),
            ),
            "templates" => {
                let names =
                    uri_template_variables(selection, self.l()).map_err(|e| fail("C_MCP", e))?;
                let fields = names
                    .into_iter()
                    .map(|n| {
                        (
                            n.to_owned(),
                            Port::new(ValueType::builtin(BuiltinType::String), true),
                        )
                    })
                    .collect();
                (
                    McpBindingKind::Template {
                        uri_template: selection.into(),
                    },
                    one_port(
                        "arguments",
                        self.make_type(ValueTypeKind::Record(fields))?,
                        self.l(),
                    )?,
                    ValueType::builtin(BuiltinType::McpResourceResult),
                )
            }
            _ => {
                let d = map(descriptor.value())?;
                let mut fields = vec![];
                if let Some(arguments) = d.get("arguments") {
                    for a in array(arguments)? {
                        let a = map(a)?;
                        let name = text(field(a, "name")?)?;
                        let required = matches!(a.get("required"), Some(Value::Bool(true)));
                        fields.push((
                            name.into(),
                            Port::new(ValueType::builtin(BuiltinType::String), required),
                        ));
                    }
                }
                (
                    McpBindingKind::Prompt {
                        name: selection.into(),
                    },
                    one_port(
                        "arguments",
                        self.make_type(ValueTypeKind::Record(fields))?,
                        self.l(),
                    )?,
                    ValueType::builtin(BuiltinType::McpPromptResult),
                )
            }
        };
        let binding = McpBinding::new(server, descriptor.digest(), kind, self.l())
            .map_err(|e| fail("C_MCP", e))?;
        let id = binding.digest(self.l()).map_err(|e| fail("C_MCP", e))?;
        self.pool.documents.insert(descriptor.digest(), descriptor);
        self.pool.bindings.insert(id, binding);
        Ok((id, inputs, one_port("value", output, self.l())?))
    }
    pub fn document(&mut self, name: &str, root: Digest) -> Result<CanonicalDocument> {
        self.charge()?;
        let compiler = self.compiler;
        let l = &compiler.config.limits;
        let mut out = DocumentFields::new(name.into(), self.compiler.config.profile.clone(), root);
        out.documents.insert(
            self.compiler.config.policy.digest(),
            self.compiler.config.policy.document().clone(),
        );
        let mut pending = BTreeSet::from([root]);
        let mut reached = BTreeSet::new();
        while let Some(id) = pending.pop_first() {
            self.charge()?;
            if !reached.insert(id) {
                continue;
            }
            if let Some(scope) = self.pool.scopes.get(&id) {
                scope_dependencies(scope, &mut pending);
                out.scopes.insert(id, scope.clone());
            } else if let Some(template) = self.pool.templates.get(&id) {
                out.templates.insert(id, template.clone());
            } else if let Some(binding) = self.pool.bindings.get(&id) {
                out.bindings.insert(id, binding.clone());
                out.documents.insert(
                    binding.descriptor(),
                    self.pool.documents[&binding.descriptor()].clone(),
                );
            } else if let Some(library) = self.pool.libraries.get(&id) {
                out.libraries.insert(id, library.clone());
            }
        }
        let mut schemas = self.catalogs.schemas.clone();
        let mut roots = BTreeSet::new();
        for binding in out.bindings.values() {
            if let McpBindingKind::Tool {
                input_schema,
                output_schema,
                ..
            } = binding.kind()
            {
                for id in [input_schema, output_schema] {
                    let doc = &self.pool.documents[id];
                    let base = crate::analyzer::embedded_schema_base(doc, l)
                        .map_err(|e| fail("C_SCHEMA", e))?;
                    if schemas.get(&base).is_some_and(|old| old != doc) {
                        return Err(fail(
                            "C_SCHEMA",
                            "conflicting embedded schema retrieval root",
                        ));
                    }
                    roots.insert(base.clone());
                    schemas.insert(base, doc.clone());
                }
            }
        }
        if !roots.is_empty() {
            let catalog = crate::analyzer::SchemaCatalog::new(schemas.into_iter().collect(), l)
                .map_err(|e| fail("C_SCHEMA", e))?;
            let roots = roots.iter().map(String::as_str).collect::<Vec<_>>();
            let closure = catalog
                .reference_closure(&roots, l)
                .map_err(|e| fail("C_SCHEMA", e))?;
            for uri in closure.retrieval_uris() {
                let doc = catalog.document(uri).expect("closure retrieval");
                out.schema_uris.insert((*uri).into(), doc.digest());
                out.documents.insert(doc.digest(), doc.clone());
            }
        }
        CanonicalDocument::new(out, l).map_err(|e| fail("C_SERIALIZATION", e))
    }
    pub fn source_error(&self, mut e: crate::CompilationFailure) -> crate::CompilationFailure {
        for d in &mut e.diagnostics {
            d.location = self.source_location(&d.location);
            d.related_locations = d
                .related_locations
                .iter()
                .map(|l| self.source_location(l))
                .collect();
        }
        e
    }
    pub(crate) fn source_location(&self, l: &DiagnosticLocation) -> DiagnosticLocation {
        if let DiagnosticLocation::CanonicalRecordPath(p) = l
            && let Some(entry) = self
                .source_map
                .iter()
                .filter(|e| {
                    p == &e.canonical_path
                        || p.strip_prefix(&e.canonical_path)
                            .is_some_and(|s| s.starts_with('/'))
                })
                .max_by_key(|e| e.canonical_path.len())
        {
            return DiagnosticLocation::SourceSpan(entry.span.clone());
        }
        l.clone()
    }
    pub fn finish(&mut self, module: usize) -> Result<crate::CompilationSuccess> {
        let graphs = self.modules[module]
            .declarations
            .iter()
            .filter(|d| matches!(d.kind, DeclKind::Graph(_)))
            .cloned()
            .collect::<Vec<_>>();
        if graphs.len() != 1 {
            return Err(fail(
                "C_SCOPE",
                "entry source must contain exactly one graph",
            ));
        }
        self.check_all()?;
        let root = self.scope_decl(module, &graphs[0].name)?;
        let doc = self.document(&graphs[0].name, root)?;
        let mut success = self
            .compiler
            .compile_document(doc, self.catalogs)
            .map_err(|e| self.source_error(e))?;
        for d in &mut success.diagnostics {
            d.location = self.source_location(&d.location);
            d.related_locations = d
                .related_locations
                .iter()
                .map(|l| self.source_location(l))
                .collect();
        }
        let value = success
            .canonical_document
            .to_value(self.l())
            .map_err(|e| fail("C_SERIALIZATION", e))?;
        success.source_map = self
            .source_map
            .iter()
            .filter(|e| {
                JsonPointer::new(&e.canonical_path, self.l())
                    .is_ok_and(|p| p.resolve_value(&value).is_ok())
            })
            .cloned()
            .collect();
        success.source_map.sort_by(|a, b| {
            a.canonical_path
                .cmp(&b.canonical_path)
                .then(a.span.cmp(&b.span))
        });
        success.source_map.dedup();
        success.documentation = self
            .documentation
            .iter()
            .filter(|(p, _)| {
                JsonPointer::new(p, self.l()).is_ok_and(|p| p.resolve_value(&value).is_ok())
            })
            .map(|(p, d)| (p.clone(), d.clone()))
            .collect();
        success
            .diagnostics
            .extend(self.diagnostics.iter().cloned().map(|mut d| {
                d.location = self.source_location(&d.location);
                d.related_locations = d
                    .related_locations
                    .iter()
                    .map(|l| self.source_location(l))
                    .collect();
                d
            }));
        crate::diagnostic::sort(&mut success.diagnostics);
        let bytes = success.source_map.iter().fold(0usize, |n, e| {
            n.saturating_add(crate::diagnostic::map_size(e))
        }) + success.documentation.iter().fold(0usize, |n, (p, d)| {
            n.saturating_add(p.len()).saturating_add(d.len())
        });
        let bytes = success.diagnostics.iter().fold(bytes, |n, d| {
            n.saturating_add(crate::diagnostic::diagnostic_size(d))
        });
        if bytes > self.compiler.config.frontend.max_report_bytes {
            return Err(fail("C_SOURCE_LIMIT", "compiler report limit exceeded"));
        }
        Ok(success)
    }
}
fn one_port(name: &str, t: ValueType, l: &Limits) -> Result<PortTable> {
    PortTable::new(vec![(identifier(name)?, Port::new(t, true))], l).map_err(|e| fail("C_TYPE", e))
}
fn type_error(e: TypeError) -> crate::CompilationFailure {
    let code = match &e {
        TypeError::DuplicateEnumMember => "C_DUPLICATE",
        TypeError::Codec(e) if matches!(e.kind(), cbor::ErrorKind::DuplicateMapKey) => {
            "C_DUPLICATE"
        }
        _ => "C_TYPE",
    };
    fail(code, e)
}
fn expression_error(e: ExpressionError) -> crate::CompilationFailure {
    let code = match &e {
        ExpressionError::DuplicateField => "C_DUPLICATE",
        ExpressionError::InvalidRegexFlags => "C_REGEX",
        ExpressionError::InvalidTemplateParameter | ExpressionError::TemplateParameterMismatch => {
            "C_PROMPT"
        }
        ExpressionError::Codec(e) if matches!(e.kind(), cbor::ErrorKind::DuplicateMapKey) => {
            "C_DUPLICATE"
        }
        _ => "C_TYPE",
    };
    fail(code, e)
}
fn graph_error(e: GraphRecordError) -> crate::CompilationFailure {
    match e {
        GraphRecordError::Type(e) => type_error(e),
        GraphRecordError::Expression(e) => expression_error(e),
        GraphRecordError::DuplicateId(_) => fail("C_DUPLICATE", e),
        GraphRecordError::InvalidBound("max_iterations") => fail("C_LOOP", e),
        GraphRecordError::Options(_) => fail("C_POLICY", e),
        GraphRecordError::Codec(ref c) if matches!(c.kind(), cbor::ErrorKind::DuplicateMapKey) => {
            fail("C_DUPLICATE", e)
        }
        _ => fail("C_SCOPE", e),
    }
}
fn scope_dependencies(scope: &Scope, out: &mut BTreeSet<Digest>) {
    let f = scope.fields();
    expression_dependencies(&f.preconditions, out);
    expression_dependencies(&f.postconditions, out);
    for e in &f.edges {
        expression_dependencies(e.guard(), out);
    }
    for n in &f.nodes {
        let n = n.fields();
        for e in [&n.guard, &n.preconditions, &n.postconditions] {
            expression_dependencies(e, out);
        }
        match &n.operation {
            Operation::Eval(e) => expression_dependencies(e, out),
            Operation::Scope(d) => {
                out.insert(*d);
            }
            Operation::Loop { body, until, .. } => {
                out.insert(*body);
                expression_dependencies(until, out);
            }
            Operation::Mcp { binding, .. } => {
                out.insert(*binding);
            }
            Operation::Wait { .. } => {}
        }
    }
}
fn expression_dependencies(e: &Expression, out: &mut BTreeSet<Digest>) {
    match e.kind() {
        ExpressionKind::FunctionRef { library, .. } => {
            out.insert(*library);
        }
        ExpressionKind::Call {
            function,
            arguments,
        } => {
            if let FunctionId::Library { library, .. } = function {
                out.insert(*library);
            }
            for e in arguments {
                expression_dependencies(e, out);
            }
        }
        ExpressionKind::Render {
            template,
            arguments,
        } => {
            out.insert(*template);
            for (_, e) in arguments {
                expression_dependencies(e, out);
            }
        }
        ExpressionKind::Get { value, .. } | ExpressionKind::Not(value) => {
            expression_dependencies(value, out)
        }
        ExpressionKind::Binary { left, right, .. } => {
            expression_dependencies(left, out);
            expression_dependencies(right, out);
        }
        ExpressionKind::List(values) => {
            for e in values {
                expression_dependencies(e, out)
            }
        }
        ExpressionKind::Record(fields) => {
            for (_, e) in fields {
                expression_dependencies(e, out)
            }
        }
        _ => {}
    }
}
fn literal_value(e: &Expr) -> Result<Value> {
    match &e.value {
        ExprKind::Literal(l) => Ok(match l {
            ScalarLiteral::String(s) => Value::Text(s.clone()),
            ScalarLiteral::Integer(n) => Value::Integer(*n),
            _ => return Err(fail("C_MCP", "invalid retry value")),
        }),
        ExprKind::List(es) => Ok(Value::Array(
            es.iter().map(literal_value).collect::<Result<_>>()?,
        )),
        _ => Err(fail("C_MCP", "retry members must be literal values")),
    }
}

impl Compiler {
    /// Compiles one complete standalone source string. Imports require a bundle.
    ///
    /// # Errors
    /// Returns source, resolution, catalog, semantic, limit or output diagnostics.
    pub fn compile_source(
        &self,
        source: &str,
        catalogs: &MCPCatalogInputs,
    ) -> Result<crate::CompilationSuccess> {
        if source.len() > self.config.frontend.max_source_bytes {
            return Err(fail("C_SOURCE_LIMIT", "source byte limit exceeded"));
        }
        let module = crate::parser::parse(
            source,
            crate::SourceLocation::Inline,
            self.config.frontend.max_tokens,
            self.config.frontend.max_depth,
            &self.config.limits,
        )?;
        if !module.imports.is_empty() {
            return Err(fail("C_IMPORT", "standalone source cannot import modules"));
        }
        let mut session = Session::new(self, catalogs, vec![module], vec![BTreeMap::new()])?;
        session.finish(0)
    }
}
