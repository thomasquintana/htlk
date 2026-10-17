//! Recursive-descent source parser. All recursive entries enforce a finite depth.
use crate::{
    SourceLocation, SourceSpan,
    ast::*,
    diagnostic::{Result, fail},
    executable::{
        BinaryOperator as Op, PathStep, ScalarLiteral,
        cbor::{FiniteFloat, Limits},
    },
    lexer::{Token, at},
};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) fn parse(
    source: &str,
    location: SourceLocation,
    max_tokens: usize,
    max_depth: usize,
    limits: &Limits,
) -> Result<Module> {
    let tokens = crate::lexer::lex(source, location.clone(), max_tokens, limits)?;
    let mut p = Parser {
        tokens,
        pos: 0,
        location,
        len: source.len(),
        depth: 0,
        max_depth,
    };
    p.expect("ir_version")?;
    p.expect("=")?;
    if p.string()? != "0.1" {
        let mut e = p.error("unsupported source version");
        e.diagnostics[0].code = "C_VERSION".into();
        return Err(e);
    }
    let mut imports = vec![];
    while p.is("import") {
        let start = p.span();
        p.pos += 1;
        let alias = p.identifier()?;
        p.expect("from")?;
        let path = p.string()?;
        imports.push(Located {
            value: (alias, path),
            span: p.finish(start),
        });
    }
    let mut declarations = vec![];
    while p.pos < p.tokens.len() {
        let start = p.span();
        let exported = p.eat("export");
        let kind = p.word()?;
        let name = p.name()?;
        let value = match kind.as_str() {
            "type" => {
                p.expect("=")?;
                DeclKind::Type(p.ty()?)
            }
            "prompt" => {
                let params = if p.eat("(") {
                    let mut params = vec![];
                    if !p.eat(")") {
                        loop {
                            let n = p.identifier()?;
                            p.expect("=")?;
                            params.push((n, p.ty()?));
                            if p.eat(")") {
                                break;
                            }
                            p.expect(",")?;
                            if p.eat(")") {
                                break;
                            }
                        }
                    }
                    Some(params)
                } else {
                    None
                };
                p.expect("=")?;
                DeclKind::Prompt(params, p.template()?)
            }
            "predicate_library" => {
                p.expect("=")?;
                p.expect("predicates")?;
                p.expect(".")?;
                p.expect("library")?;
                p.expect("(")?;
                let id = p.string()?;
                p.expect(")")?;
                p.expect("{")?;
                let mut m = BTreeMap::new();
                while !p.eat("}") {
                    let k = p.word()?;
                    p.expect("=")?;
                    let v = p.string()?;
                    if !matches!(k.as_str(), "version" | "digest") || m.insert(k, v).is_some() {
                        return Err(p.error("unknown or duplicate library member"));
                    }
                }
                let version = m
                    .remove("version")
                    .ok_or_else(|| p.error("missing library version"))?;
                let digest = m
                    .remove("digest")
                    .ok_or_else(|| p.error("missing library digest"))?;
                DeclKind::Library(id, version, digest)
            }
            "task" => DeclKind::Task(p.block("scope")?),
            "graph" => DeclKind::Graph(p.block("scope")?),
            _ => return Err(p.error("expected declaration")),
        };
        if exported && matches!(value, DeclKind::Library(..) | DeclKind::Graph(..)) {
            return Err(p.error("only type, prompt and task can be exported"));
        }
        declarations.push(Decl {
            name,
            exported,
            kind: value,
            span: p.finish(start),
        });
    }
    Ok(Module {
        tokens: p.tokens.len(),
        imports,
        declarations,
    })
}
struct Parser {
    tokens: Vec<Located<Token>>,
    pos: usize,
    location: SourceLocation,
    len: usize,
    depth: usize,
    max_depth: usize,
}
impl Parser {
    fn span(&self) -> SourceSpan {
        self.tokens
            .get(self.pos)
            .map(|t| t.span.clone())
            .unwrap_or(SourceSpan {
                source: self.location.clone(),
                start_byte: self.len,
                end_byte: self.len,
            })
    }
    fn finish(&self, mut s: SourceSpan) -> SourceSpan {
        s.end_byte = self
            .tokens
            .get(self.pos.saturating_sub(1))
            .map_or(s.end_byte, |t| t.span.end_byte);
        s
    }
    fn error(&self, m: &str) -> crate::CompilationFailure {
        let s = self.span();
        at(m, s.source, s.start_byte, s.end_byte)
    }
    fn duplicate(&self, m: &str) -> crate::CompilationFailure {
        let mut e = self.error(m);
        e.diagnostics[0].code = "C_DUPLICATE".into();
        e
    }
    fn is(&self, s: &str) -> bool {
        matches!(self.tokens.get(self.pos).map(|t|&t.value),Some(Token::Word(w)|Token::Symbol(w)) if w==s)
    }
    fn eat(&mut self, s: &str) -> bool {
        if self.is(s) {
            self.pos += 1;
            true
        } else {
            false
        }
    }
    fn expect(&mut self, s: &str) -> Result<()> {
        if self.eat(s) {
            Ok(())
        } else {
            Err(self.error(&format!("expected {s}")))
        }
    }
    fn word(&mut self) -> Result<String> {
        if let Some(Located {
            value: Token::Word(s),
            ..
        }) = self.tokens.get(self.pos)
        {
            let s = s.clone();
            self.pos += 1;
            Ok(s)
        } else {
            Err(self.error("expected name"))
        }
    }
    fn identifier(&mut self) -> Result<String> {
        let span = self.span();
        let s = self.word()?;
        s.parse::<crate::executable::Identifier>().map_err(|_| {
            at(
                "invalid identifier",
                span.source,
                span.start_byte,
                span.end_byte,
            )
        })?;
        Ok(s)
    }
    fn name(&mut self) -> Result<String> {
        let mut s = self.word()?;
        while self.eat(".") {
            s.push('.');
            s.push_str(&self.word()?);
        }
        Ok(s)
    }
    fn string(&mut self) -> Result<String> {
        if let Some(Located {
            value: Token::String(s),
            ..
        }) = self.tokens.get(self.pos)
        {
            let s = s.clone();
            self.pos += 1;
            Ok(s)
        } else {
            Err(self.error("expected string literal"))
        }
    }
    fn integer(&mut self) -> Result<i64> {
        if let Some(Located {
            value: Token::Number(s),
            ..
        }) = self.tokens.get(self.pos)
        {
            let n = s
                .parse()
                .map_err(|_| self.error("expected signed-i64 integer"))?;
            self.pos += 1;
            Ok(n)
        } else {
            Err(self.error("expected integer"))
        }
    }
    fn template(&mut self) -> Result<String> {
        if let Some(Located {
            value: Token::TripleString(s),
            ..
        }) = self.tokens.get(self.pos)
        {
            let s = s.clone();
            self.pos += 1;
            Ok(s)
        } else {
            self.string()
        }
    }
    fn key(&mut self) -> Result<String> {
        if matches!(
            self.tokens.get(self.pos).map(|t| &t.value),
            Some(Token::String(_))
        ) {
            self.string()
        } else {
            self.identifier()
        }
    }
    fn enter(&mut self) -> Result<()> {
        self.depth += 1;
        if self.depth > self.max_depth {
            Err(fail("C_SOURCE_LIMIT", "frontend nesting limit exceeded"))
        } else {
            Ok(())
        }
    }
    fn ports(&mut self) -> Result<Ports> {
        self.expect("{")?;
        let mut fields = vec![];
        let mut names = BTreeSet::new();
        if !self.eat("}") {
            loop {
                let k = self.key()?;
                if !names.insert(k.clone()) {
                    return Err(self.duplicate("duplicate decoded port/field name"));
                }
                self.expect("=")?;
                fields.push((k, self.port_ty()?));
                if self.eat("}") {
                    break;
                }
                self.expect(",")?;
                if self.eat("}") {
                    break;
                }
            }
        }
        Ok(fields)
    }
    fn port_ty(&mut self) -> Result<PortTy> {
        let optional = self.eat("optional");
        if optional {
            self.expect("(")?;
        }
        let ty = self.ty()?;
        if optional {
            self.expect(")")?;
        }
        Ok(PortTy {
            ty,
            required: !optional,
        })
    }
    fn ty(&mut self) -> Result<Ty> {
        self.enter()?;
        let n = self.name()?;
        let ty = match n.as_str() {
            "list" | "map" => {
                self.expect("(")?;
                let inner = Box::new(self.ty()?);
                self.expect(")")?;
                if n == "list" {
                    Ty::List(inner)
                } else {
                    Ty::Map(inner)
                }
            }
            "record" => Ty::Record(self.ports()?),
            "union" => {
                self.expect("(")?;
                let mut members = vec![self.ty()?];
                while self.eat(",") {
                    if self.is(")") {
                        break;
                    }
                    members.push(self.ty()?);
                }
                self.expect(")")?;
                Ty::Union(members)
            }
            "enum" => {
                self.expect("(")?;
                let mut members = vec![self.string()?];
                while self.eat(",") {
                    if self.is(")") {
                        break;
                    }
                    members.push(self.string()?);
                }
                self.expect(")")?;
                Ty::Enum(members)
            }
            "optional" => return Err(self.error("optional is only permitted on ports/fields")),
            _ => Ty::Name(n),
        };
        self.depth -= 1;
        Ok(ty)
    }
    fn block(&mut self, role: &str) -> Result<Block> {
        self.enter()?;
        self.expect("{")?;
        let mut b = Block::default();
        let mut seen = BTreeSet::new();
        while !self.eat("}") {
            let key = self.word()?;
            if !seen.insert(key.clone()) {
                return Err(self.duplicate("duplicate block member"));
            }
            let legal = match role {
                "scope" => matches!(
                    key.as_str(),
                    "description"
                        | "inputs"
                        | "outputs"
                        | "nodes"
                        | "edges"
                        | "preconditions"
                        | "postconditions"
                        | "limits"
                ),
                "body" => matches!(key.as_str(), "nodes" | "edges"),
                "loop" => matches!(
                    key.as_str(),
                    "description"
                        | "inputs"
                        | "outputs"
                        | "when"
                        | "preconditions"
                        | "postconditions"
                        | "limits"
                        | "carried"
                        | "body"
                        | "until"
                        | "max_iterations"
                ),
                _ => matches!(
                    key.as_str(),
                    "description"
                        | "when"
                        | "preconditions"
                        | "postconditions"
                        | "limits"
                        | "inputs"
                        | "retry"
                        | "pin"
                        | "topic"
                        | "timeout_ms"
                ),
            };
            if !legal {
                return Err(self.error("unknown block member"));
            }
            match key.as_str() {
                "inputs" | "outputs" => {
                    self.expect("=")?;
                    let ports = self.ports()?;
                    if key == "inputs" {
                        b.inputs = Some(ports)
                    } else {
                        b.outputs = Some(ports)
                    }
                }
                "description" | "pin" | "topic" => {
                    self.expect("=")?;
                    let s = self.string()?;
                    match key.as_str() {
                        "description" => b.description = Some(s),
                        "pin" => b.pin = Some(s),
                        _ => b.topic = Some(s),
                    }
                }
                "when" | "preconditions" | "postconditions" | "until" => {
                    self.expect("=")?;
                    let e = self.expr(0)?;
                    match key.as_str() {
                        "when" => b.guard = Some(e),
                        "preconditions" => b.preconditions = Some(e),
                        "postconditions" => b.postconditions = Some(e),
                        _ => b.until = Some(e),
                    }
                }
                "timeout_ms" | "max_iterations" => {
                    self.expect("=")?;
                    let n = self.integer()?;
                    if key == "timeout_ms" {
                        b.timeout_ms = Some(n)
                    } else {
                        b.max_iterations = Some(n)
                    }
                }
                "limits" => {
                    self.expect("{")?;
                    let mut m = BTreeMap::new();
                    while !self.eat("}") {
                        let k = self.identifier()?;
                        self.expect("=")?;
                        let v = self.integer()?;
                        if m.insert(k, v).is_some() {
                            return Err(self.duplicate("duplicate limit"));
                        }
                    }
                    b.limits = Some(m)
                }
                "retry" => {
                    self.expect("{")?;
                    let mut m = BTreeMap::new();
                    while !self.eat("}") {
                        let k = self.word()?;
                        self.expect("=")?;
                        let v = self.expr(0)?;
                        if !matches!(k.as_str(), "max_attempts" | "on" | "backoff_ms")
                            || m.insert(k, v).is_some()
                        {
                            return Err(self.error("unknown or duplicate retry member"));
                        }
                    }
                    if m.len() != 3 {
                        return Err(self.error("incomplete retry policy"));
                    }
                    b.retry = Some(m)
                }
                "carried" => {
                    self.expect("=")?;
                    self.expect("{")?;
                    let mut m = vec![];
                    if !self.eat("}") {
                        loop {
                            let k = self.identifier()?;
                            self.expect("=")?;
                            self.expect("inputs")?;
                            self.expect(".")?;
                            m.push((k, self.identifier()?));
                            if self.eat("}") {
                                break;
                            }
                            self.expect(",")?;
                            if self.eat("}") {
                                break;
                            }
                        }
                    }
                    b.carried = Some(m)
                }
                "body" => b.body = Some(Box::new(self.block("body")?)),
                "nodes" => {
                    self.expect("{")?;
                    let mut nodes = vec![];
                    while !self.eat("}") {
                        let start = self.span();
                        let id = self.identifier()?;
                        self.expect("=")?;
                        let op = self.word()?;
                        let (operation, options) = if op == "loop" {
                            (AstOperation::Loop, self.block("loop")?)
                        } else {
                            self.expect("(")?;
                            let operation = match op.as_str() {
                                "eval" => {
                                    let ty = self.port_ty()?;
                                    self.expect(",")?;
                                    AstOperation::Eval(ty, self.expr(0)?)
                                }
                                "use" => AstOperation::Use(self.name()?),
                                "wait" => AstOperation::Wait(self.ty()?),
                                "call" | "read" | "fetch" => {
                                    self.expect("mcp")?;
                                    self.expect(".")?;
                                    let category = self.word()?;
                                    if !matches!(
                                        (op.as_str(), category.as_str()),
                                        ("call", "tool")
                                            | ("read", "resource" | "template")
                                            | ("fetch", "prompt")
                                    ) {
                                        return Err(self.error("invalid MCP operation selector"));
                                    }
                                    self.expect("(")?;
                                    let server = self.string()?;
                                    self.expect(",")?;
                                    let selection = self.string()?;
                                    self.expect(")")?;
                                    AstOperation::Mcp(category, server, selection)
                                }
                                _ => return Err(self.error("unknown operation")),
                            };
                            self.expect(")")?;
                            let options = if self.is("{") {
                                self.block("node")?
                            } else {
                                Block::default()
                            };
                            (operation, options)
                        };
                        nodes.push(Located {
                            value: AstNode {
                                id,
                                operation,
                                options,
                            },
                            span: self.finish(start),
                        });
                    }
                    b.nodes = Some(nodes)
                }
                "edges" => {
                    self.expect("{")?;
                    let mut edges = vec![];
                    while !self.eat("}") {
                        let start = self.span();
                        self.expect("edge")?;
                        let id = self.identifier()?;
                        self.expect("{")?;
                        let mut from = None;
                        let mut to = None;
                        let mut guard = None;
                        let mut description = None;
                        let mut seen = BTreeSet::new();
                        while !self.eat("}") {
                            let k = self.word()?;
                            if !seen.insert(k.clone()) {
                                return Err(self.duplicate("duplicate edge member"));
                            }
                            self.expect("=")?;
                            match k.as_str() {
                                "from" => {
                                    from =
                                        Some(self.name()?.split('.').map(str::to_owned).collect())
                                }
                                "to" => {
                                    to = Some(self.name()?.split('.').map(str::to_owned).collect())
                                }
                                "when" => guard = Some(self.expr(0)?),
                                "description" => description = Some(self.string()?),
                                _ => return Err(self.error("unknown edge member")),
                            }
                        }
                        edges.push(Located {
                            value: AstEdge {
                                id,
                                from: from.ok_or_else(|| self.error("missing edge source"))?,
                                to: to.ok_or_else(|| self.error("missing edge destination"))?,
                                guard,
                                description,
                            },
                            span: self.finish(start),
                        });
                    }
                    b.edges = Some(edges)
                }
                _ => unreachable!(),
            }
        }
        let required = match role {
            "scope" => &["inputs", "outputs", "nodes", "edges"][..],
            "loop" => &[
                "inputs",
                "outputs",
                "carried",
                "body",
                "until",
                "max_iterations",
            ][..],
            "body" => &["nodes", "edges"][..],
            _ => &[][..],
        };
        if required.iter().any(|k| !seen.contains(*k)) {
            return Err(self.error("missing required block member"));
        }
        self.depth -= 1;
        Ok(b)
    }
    fn expr(&mut self, min: u8) -> Result<Expr> {
        self.enter()?;
        let start = self.span();
        let mut left = if self.eat("not") {
            Located {
                value: ExprKind::Not(Box::new(self.expr(3)?)),
                span: self.finish(start.clone()),
            }
        } else {
            self.primary()?
        };
        let mut compared = false;
        let mut chain = 0;
        loop {
            let op = if self.is("or") {
                Some((1, Op::Or))
            } else if self.is("and") {
                Some((2, Op::And))
            } else {
                [
                    ("==", Op::Eq),
                    ("!=", Op::Ne),
                    ("<", Op::Lt),
                    ("<=", Op::Le),
                    (">", Op::Gt),
                    (">=", Op::Ge),
                ]
                .into_iter()
                .find(|(s, _)| self.is(s))
                .map(|(_, o)| (4, o))
            };
            let Some((precedence, operator)) = op else {
                break;
            };
            if precedence < min {
                break;
            }
            chain += 1;
            if chain + self.depth > self.max_depth {
                return Err(fail("C_SOURCE_LIMIT", "expression chain nesting exceeded"));
            }
            if precedence == 4
                && self
                    .tokens
                    .get(self.pos + 1)
                    .is_some_and(|t| matches!(&t.value,Token::Word(s) if s=="not"))
            {
                return Err(self.error("comparison operands must be primary expressions"));
            }
            if precedence == 4 && compared {
                return Err(self.error("chained comparisons are not permitted"));
            }
            compared |= precedence == 4;
            self.pos += 1;
            let right = self.expr(precedence + 1)?;
            left = Located {
                value: ExprKind::Binary(operator, Box::new(left), Box::new(right)),
                span: self.finish(start.clone()),
            };
        }
        self.depth -= 1;
        Ok(left)
    }
    fn primary(&mut self) -> Result<Expr> {
        let start = self.span();
        let value = if self.eat("(") {
            let e = self.expr(0)?;
            self.expect(")")?;
            e.value
        } else if self.eat("&") {
            ExprKind::Static(self.name()?.split('.').map(str::to_owned).collect())
        } else if self.eat("@") {
            ExprKind::Handle(self.identifier()?)
        } else if self.eat("[") {
            let mut a = vec![];
            if !self.eat("]") {
                loop {
                    a.push(self.expr(0)?);
                    if self.eat("]") {
                        break;
                    }
                    self.expect(",")?;
                    if self.eat("]") {
                        break;
                    }
                }
            }
            ExprKind::List(a)
        } else if self.eat("{") {
            let mut a = vec![];
            if !self.eat("}") {
                loop {
                    let k = self.key()?;
                    self.expect("=")?;
                    a.push((k, self.expr(0)?));
                    if self.eat("}") {
                        break;
                    }
                    self.expect(",")?;
                    if self.eat("}") {
                        break;
                    }
                }
            }
            ExprKind::Record(a)
        } else {
            let token = self
                .tokens
                .get(self.pos)
                .ok_or_else(|| self.error("expected expression"))?
                .value
                .clone();
            self.pos += 1;
            match token {
                Token::String(s) | Token::TripleString(s) => {
                    ExprKind::Literal(ScalarLiteral::String(s))
                }
                Token::Regex(p, f) => ExprKind::Regex(p, f),
                Token::Number(n) => {
                    let lit = if n.contains(['.', 'e', 'E']) {
                        ScalarLiteral::Float(
                            FiniteFloat::new(n.parse().map_err(|_| self.error("invalid float"))?)
                                .map_err(|_| self.error("nonfinite float"))?,
                        )
                    } else {
                        ScalarLiteral::Integer(
                            n.parse()
                                .map_err(|_| self.error("integer outside signed-i64 range"))?,
                        )
                    };
                    ExprKind::Literal(lit)
                }
                Token::Word(w) => match w.as_str() {
                    "true" => ExprKind::Literal(ScalarLiteral::Boolean(true)),
                    "false" => ExprKind::Literal(ScalarLiteral::Boolean(false)),
                    "null" => ExprKind::Literal(ScalarLiteral::Null),
                    _ => {
                        let mut name = vec![w];
                        while self.eat(".") {
                            name.push(self.identifier()?);
                        }
                        if self.eat("(") {
                            let mut args = vec![];
                            if !self.eat(")") {
                                loop {
                                    args.push(self.expr(0)?);
                                    if self.eat(")") {
                                        break;
                                    }
                                    self.expect(",")?;
                                    if self.eat(")") {
                                        break;
                                    }
                                }
                            }
                            ExprKind::Call(name, args)
                        } else {
                            ExprKind::Name(name)
                        }
                    }
                },
                _ => return Err(self.error("expected expression")),
            }
        };
        let mut e = Located {
            value,
            span: self.finish(start.clone()),
        };
        let mut suffixes = 0;
        loop {
            let step = if self.eat(".") {
                Some(PathStep::Field(self.identifier()?))
            } else if self.eat("[") {
                let step = if matches!(
                    self.tokens.get(self.pos).map(|t| &t.value),
                    Some(Token::String(_))
                ) {
                    PathStep::Field(self.string()?)
                } else {
                    PathStep::Index(
                        self.integer()?
                            .try_into()
                            .map_err(|_| self.error("negative projection index"))?,
                    )
                };
                self.expect("]")?;
                Some(step)
            } else {
                None
            };
            let Some(step) = step else {
                break;
            };
            suffixes += 1;
            if suffixes + self.depth > self.max_depth {
                return Err(fail("C_SOURCE_LIMIT", "projection chain nesting exceeded"));
            }
            e = Located {
                value: ExprKind::Get(Box::new(e), step),
                span: self.finish(start.clone()),
            };
        }
        Ok(e)
    }
}
