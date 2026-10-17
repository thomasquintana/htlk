//! Temporary source syntax. Names and source-only descriptions are erased on lowering.
use crate::{
    SourceSpan,
    executable::{BinaryOperator, ScalarLiteral},
};
use std::collections::BTreeMap;

#[derive(Clone, Debug)]
pub(crate) struct Located<T> {
    pub value: T,
    pub span: SourceSpan,
}
pub(crate) type Expr = Located<ExprKind>;
#[derive(Clone, Debug)]
pub(crate) enum ExprKind {
    Literal(ScalarLiteral),
    Regex(String, String),
    Name(Vec<String>),
    Call(Vec<String>, Vec<Expr>),
    Static(Vec<String>),
    Handle(String),
    List(Vec<Expr>),
    Record(Vec<(String, Expr)>),
    Get(Box<Expr>, crate::executable::PathStep),
    Not(Box<Expr>),
    Binary(BinaryOperator, Box<Expr>, Box<Expr>),
}
#[derive(Clone, Debug)]
pub(crate) enum Ty {
    Name(String),
    List(Box<Ty>),
    Map(Box<Ty>),
    Union(Vec<Ty>),
    Enum(Vec<String>),
    Record(Vec<(String, PortTy)>),
}
#[derive(Clone, Debug)]
pub(crate) struct PortTy {
    pub ty: Ty,
    pub required: bool,
}
pub(crate) type Ports = Vec<(String, PortTy)>;

#[derive(Clone, Debug, Default)]
pub(crate) struct Block {
    pub inputs: Option<Ports>,
    pub outputs: Option<Ports>,
    pub nodes: Option<Vec<Located<AstNode>>>,
    pub edges: Option<Vec<Located<AstEdge>>>,
    pub description: Option<String>,
    pub guard: Option<Expr>,
    pub preconditions: Option<Expr>,
    pub postconditions: Option<Expr>,
    pub limits: Option<BTreeMap<String, i64>>,
    pub retry: Option<BTreeMap<String, Expr>>,
    pub pin: Option<String>,
    pub topic: Option<String>,
    pub timeout_ms: Option<i64>,
    pub carried: Option<Vec<(String, String)>>,
    pub body: Option<Box<Block>>,
    pub until: Option<Expr>,
    pub max_iterations: Option<i64>,
}
#[derive(Clone, Debug)]
pub(crate) enum AstOperation {
    Eval(PortTy, Expr),
    Mcp(String, String, String),
    Use(String),
    Wait(Ty),
    Loop,
}
#[derive(Clone, Debug)]
pub(crate) struct AstNode {
    pub id: String,
    pub operation: AstOperation,
    pub options: Block,
}
#[derive(Clone, Debug)]
pub(crate) struct AstEdge {
    pub id: String,
    pub from: Vec<String>,
    pub to: Vec<String>,
    pub guard: Option<Expr>,
    pub description: Option<String>,
}
#[derive(Clone, Debug)]
pub(crate) enum DeclKind {
    Type(Ty),
    Prompt(Option<Vec<(String, Ty)>>, String),
    Library(String, String, String),
    Task(Block),
    Graph(Block),
}
#[derive(Clone, Debug)]
pub(crate) struct Decl {
    pub name: String,
    pub exported: bool,
    pub kind: DeclKind,
    pub span: SourceSpan,
}
#[derive(Clone, Debug)]
pub(crate) struct Module {
    pub tokens: usize,
    pub imports: Vec<Located<(String, String)>>,
    pub declarations: Vec<Decl>,
}
